//! Reference desktop backend for the file-picker plugin.
//!
//! Wraps [`rfd`](https://crates.io/crates/rfd), which on Windows uses
//! `IFileOpenDialog` / `IFileSaveDialog`, on macOS uses `NSOpenPanel` /
//! `NSSavePanel`, and on Linux uses the XDG Desktop Portal
//! (`org.freedesktop.portal.FileChooser`) via `ashpd` + `zbus` — rfd
//! drives the portal's async D-Bus IO with an internal pollster
//! executor, so no external async runtime is required.
//!
//! # Blocking
//!
//! The `FilePicker` methods here are `async fn` for wire compatibility
//! but the body blocks the calling thread until the native modal loop
//! (Windows/macOS) or the portal round-trip (Linux) completes. Do not
//! call from a Tokio worker without `spawn_blocking`.
//!
//! # Handle registry
//!
//! Picked paths are stored in an in-process `HashMap<u64, PathBuf>`
//! keyed by ids from [`Runtime::alloc_native_handle_id`], so they never
//! collide with ids other hosts hand out. The map entry goes away when
//! the client drops the handle (see
//! [`DesktopFilePicker::install_release_hook`]);
//! [`DesktopFilePicker::purge`] clears it wholesale. File descriptors
//! handed out via [`crate::FilePicker::open_read`] /
//! [`crate::FilePicker::open_write`] are owned by the caller through
//! their [`std::fs::File`] wrappers and are unaffected.
//!
//! # Platform limits
//!
//! - **Filters**: `rfd` filters by extension only.
//!   [`FileFilter::extensions`] are used as given and
//!   [`FileFilter::mime_types`] (plus [`SaveConfig::mime_type`] for save
//!   dialogs) are translated through a built-in table of common types,
//!   `type/*` wildcards included. [`FileFilter::uti_types`] are ignored.
//!   A MIME type the table does not know adds nothing; when nothing at
//!   all is left, or a `*/*` is present, the dialog offers every file.
//! - **Multi-select**: [`FilePicker::pick_files`] honours
//!   [`PickConfig::allow_multiple`]; without it the dialog is
//!   single-select and returns at most one file.
//! - **Overwrite confirmation**: every save dialog `rfd` drives
//!   (`IFileSaveDialog`, `NSSavePanel`, the XDG portal and its zenity
//!   fallback) confirms overwrites itself, so
//!   [`SaveConfig::overwrite_prompt`] `= false` cannot turn the prompt
//!   off.
//! - **MIME of picked files**: [`PickedFile::mime_type`] is guessed from
//!   the extension through the same table.

mod mime;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use istmo_core::sync::lock;
use istmo_core::{NativeHandleId, Runtime};

use crate::{
    FileFilter, FilePicker, FilePickerError, PickConfig, PickedFile, RawFileHandle, SaveConfig,
};

/// Reference `FilePicker` backend for desktop targets.
///
/// Backed by an `Arc<Inner>` so a single backend can be cloned into the
/// macro-emitted `FilePickerHost` and simultaneously held by the caller
/// for release-hook installation:
///
/// ```ignore
/// let backend = DesktopFilePicker::new();
/// let init = Runtime::mock()
///     .expects::<FilePickerClient>()
///     .host(FilePickerHost::new(backend.clone()))
///     .finish();
/// backend.install_release_hook(&init.runtime);
/// ```
///
/// The release hook clears the internal `NativeHandleId -> PathBuf` map
/// when the Rust client drops a [`PickedFile`]'s
/// [`NativeHandle`](istmo_core::NativeHandle). File descriptors already
/// handed out through [`FilePicker::open_read`] / [`FilePicker::open_write`]
/// are unaffected — the caller owns them through their `std::fs::File`
/// wrappers.
#[derive(Debug, Default, Clone)]
pub struct DesktopFilePicker {
    inner: Arc<DesktopFilePickerInner>,
}

#[derive(Debug, Default)]
struct DesktopFilePickerInner {
    files: Mutex<HashMap<u64, PathBuf>>,
    /// Runtime the ids come from; set by `install_release_hook`.
    runtime: OnceLock<Weak<Runtime>>,
}

impl DesktopFilePicker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attach the picker to `runtime`: handle ids are allocated from it,
    /// and a hook removes the cached path when the client drops the
    /// corresponding [`NativeHandle`](istmo_core::NativeHandle).
    ///
    /// Must be called before the first pick — until then picks fail
    /// with [`FilePickerError::Backend`]. Installing again registers a
    /// second hook that no-ops on already-gone entries; the first
    /// runtime stays the id source.
    pub fn install_release_hook(&self, runtime: &Arc<Runtime>) {
        let _ = self.inner.runtime.set(Arc::downgrade(runtime));
        let weak = Arc::downgrade(&self.inner);
        runtime.install_native_handle_release_hook(Arc::new(move |id| {
            // Hooks see every release in the runtime; only Rust-allocated
            // ids can be ours.
            if !id.is_rust_allocated() {
                return;
            }
            if let Some(inner) = weak.upgrade() {
                lock(&inner.files).remove(&id.get());
            }
        }));
    }

    /// Drop the entire in-process URI/path map. Manual escape hatch
    /// when the release hook is not wired up.
    pub fn purge(&self) {
        lock(&self.inner.files).clear();
    }

    fn alloc(&self, path: PathBuf) -> Result<NativeHandleId, FilePickerError> {
        let runtime = self
            .inner
            .runtime
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                FilePickerError::Backend(
                    "DesktopFilePicker is not attached to a runtime; call install_release_hook"
                        .into(),
                )
            })?;
        let id = runtime.alloc_native_handle_id();
        lock(&self.inner.files).insert(id.get(), path);
        Ok(id)
    }

    fn alloc_many(
        &self,
        paths: Vec<PathBuf>,
    ) -> Result<Vec<(PathBuf, NativeHandleId)>, FilePickerError> {
        paths
            .into_iter()
            .map(|p| {
                let id = self.alloc(p.clone())?;
                Ok((p, id))
            })
            .collect()
    }

    fn lookup(&self, id: NativeHandleId) -> Result<PathBuf, FilePickerError> {
        lock(&self.inner.files)
            .get(&id.get())
            .cloned()
            .ok_or_else(|| FilePickerError::NotFound(format!("handle {}", id.get())))
    }
}

fn picked_from_path(path: &Path, handle: NativeHandleId) -> PickedFile {
    let display_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let size = std::fs::metadata(path).ok().map(|m| m.len());
    let mime_type = path
        .extension()
        .and_then(|ext| ext.to_str())
        .and_then(mime::for_extension)
        .map(str::to_owned);
    PickedFile {
        display_name,
        mime_type,
        size,
        handle,
    }
}

fn map_io<T>(res: std::io::Result<T>, ctx: &str) -> Result<T, FilePickerError> {
    res.map_err(|e| FilePickerError::Io(format!("{ctx}: {e}")))
}

fn raw_from_file(f: std::fs::File) -> i64 {
    #[cfg(unix)]
    {
        use std::os::fd::IntoRawFd;
        i64::from(f.into_raw_fd())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::IntoRawHandle;
        (f.into_raw_handle() as usize) as i64
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = f;
        panic!("target does not support fd bridging")
    }
}

// ------------------------------------------------------------------ Sync rfd (all desktop targets)

/// Extensions a dialog offers for `filter` plus `extra_mime`, or `None`
/// for "any file". See the module docs for how MIME types map.
fn filter_extensions(filter: &FileFilter, extra_mime: Option<&str>) -> Option<Vec<String>> {
    let mut exts: Vec<String> = Vec::new();
    let mut push = |ext: &str| {
        let ext = ext.trim_start_matches('.');
        if !ext.is_empty() && !exts.iter().any(|e| e.eq_ignore_ascii_case(ext)) {
            exts.push(ext.to_owned());
        }
    };
    filter.extensions.iter().for_each(|ext| push(ext));
    for mime in filter
        .mime_types
        .iter()
        .map(String::as_str)
        .chain(extra_mime)
    {
        match mime::MimeExtensions::of(mime) {
            mime::MimeExtensions::Any => return None,
            known @ mime::MimeExtensions::Known(_) => known.extensions().for_each(&mut push),
        }
    }
    (!exts.is_empty()).then_some(exts)
}

fn apply_filter(d: rfd::FileDialog, extensions: Option<&[String]>) -> rfd::FileDialog {
    match extensions {
        Some(exts) => d.add_filter("Supported files", exts),
        None => d,
    }
}

/// `name` with the canonical extension of `mime` appended when it has
/// no extension yet.
fn name_with_extension(name: &str, mime: Option<&str>) -> String {
    if Path::new(name).extension().is_some() {
        return name.to_owned();
    }
    mime.and_then(|mime| mime::MimeExtensions::of(mime).extensions().next())
        .map_or_else(|| name.to_owned(), |ext| format!("{name}.{ext}"))
}

fn build_pick_dialog(config: &PickConfig) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if let Some(t) = &config.dialog_title {
        d = d.set_title(t);
    }
    if let Some(dir) = &config.start_directory_hint {
        d = d.set_directory(dir);
    }
    apply_filter(d, filter_extensions(&config.filter, None).as_deref())
}

fn build_save_dialog(config: &SaveConfig) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if let Some(t) = &config.dialog_title {
        d = d.set_title(t);
    }
    if let Some(dir) = &config.start_directory_hint {
        d = d.set_directory(dir);
    }
    let mime = config.mime_type.as_deref();
    if let Some(name) = &config.suggested_name {
        d = d.set_file_name(name_with_extension(name, mime));
    }
    apply_filter(d, filter_extensions(&config.filter, mime).as_deref())
}

fn run_pick_single(config: &PickConfig) -> Option<PathBuf> {
    build_pick_dialog(config).pick_file()
}

fn run_pick_multi(config: &PickConfig) -> Vec<PathBuf> {
    if config.allow_multiple {
        build_pick_dialog(config).pick_files().unwrap_or_default()
    } else {
        run_pick_single(config).into_iter().collect()
    }
}

fn run_save(config: &SaveConfig) -> Option<PathBuf> {
    build_save_dialog(config).save_file()
}

// ------------------------------------------------------------------ FilePicker impl

impl FilePicker for DesktopFilePicker {
    async fn pick_file(&self, config: PickConfig) -> Result<Option<PickedFile>, FilePickerError> {
        let Some(path) = run_pick_single(&config) else {
            return Ok(None);
        };
        let handle = self.alloc(path.clone())?;
        Ok(Some(picked_from_path(&path, handle)))
    }

    async fn pick_files(&self, config: PickConfig) -> Result<Vec<PickedFile>, FilePickerError> {
        let paths = run_pick_multi(&config);
        Ok(self
            .alloc_many(paths)?
            .into_iter()
            .map(|(p, id)| picked_from_path(&p, id))
            .collect())
    }

    async fn save_file(&self, config: SaveConfig) -> Result<Option<PickedFile>, FilePickerError> {
        let Some(path) = run_save(&config) else {
            return Ok(None);
        };
        // Create the file up front so downstream open_write always
        // succeeds against a real inode.
        map_io(
            std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .map(|_| ()),
            "create save target",
        )?;
        let handle = self.alloc(path.clone())?;
        Ok(Some(picked_from_path(&path, handle)))
    }

    async fn open_read(&self, file: NativeHandleId) -> Result<RawFileHandle, FilePickerError> {
        let path = self.lookup(file)?;
        let f = map_io(std::fs::File::open(&path), "open_read")?;
        Ok(RawFileHandle {
            raw: raw_from_file(f),
        })
    }

    async fn open_write(&self, file: NativeHandleId) -> Result<RawFileHandle, FilePickerError> {
        let path = self.lookup(file)?;
        let f = map_io(
            std::fs::OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&path),
            "open_write",
        )?;
        Ok(RawFileHandle {
            raw: raw_from_file(f),
        })
    }

    async fn path(&self, file: NativeHandleId) -> Result<Option<String>, FilePickerError> {
        let path = self.lookup(file)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::{filter_extensions, name_with_extension};
    use crate::FileFilter;

    fn filter(extensions: &[&str], mime_types: &[&str]) -> FileFilter {
        FileFilter {
            mime_types: mime_types.iter().map(|m| (*m).to_owned()).collect(),
            extensions: extensions.iter().map(|e| (*e).to_owned()).collect(),
            uti_types: vec!["public.image".to_owned()],
        }
    }

    #[test]
    fn empty_filter_offers_any_file() {
        assert_eq!(filter_extensions(&FileFilter::default(), None), None);
        // UTIs alone mean nothing to the desktop dialogs.
        assert_eq!(filter_extensions(&filter(&[], &[]), None), None);
    }

    #[test]
    fn extensions_and_mime_types_merge() {
        let exts = filter_extensions(
            &filter(&[".CSV", "txt"], &["application/pdf", "text/csv"]),
            None,
        );
        assert_eq!(
            exts.as_deref(),
            Some(&["CSV", "txt", "pdf"].map(str::to_owned)[..])
        );
    }

    #[test]
    fn mime_wildcard_expands() {
        let exts = filter_extensions(&filter(&[], &["image/*"]), None).unwrap_or_default();
        assert!(exts.iter().any(|e| e == "png") && exts.iter().any(|e| e == "jpg"));
    }

    #[test]
    fn any_mime_or_only_unknown_mimes_offer_any_file() {
        assert_eq!(filter_extensions(&filter(&["png"], &["*/*"]), None), None);
        assert_eq!(
            filter_extensions(&filter(&[], &["application/x-nope"]), None),
            None
        );
    }

    #[test]
    fn save_mime_type_joins_the_filter() {
        let exts = filter_extensions(&FileFilter::default(), Some("text/csv"));
        assert_eq!(exts.as_deref(), Some(&["csv".to_owned()][..]));
    }

    #[test]
    fn suggested_name_gets_the_mime_extension() {
        assert_eq!(
            name_with_extension("export", Some("text/csv")),
            "export.csv"
        );
        assert_eq!(
            name_with_extension("export.txt", Some("text/csv")),
            "export.txt"
        );
        assert_eq!(name_with_extension("export", None), "export");
        assert_eq!(
            name_with_extension("export", Some("application/x-nope")),
            "export"
        );
    }
}
