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
//! keyed by a monotonically increasing [`NativeHandleId`]. There is no
//! automatic release path in the Rust runtime today — the map grows for
//! the process's lifetime. Call [`DesktopFilePicker::purge`] to clear it
//! (the file descriptors handed out via [`crate::FilePicker::open_read`]
//! / [`crate::FilePicker::open_write`] are owned by the caller through
//! their [`std::fs::File`] wrappers and are unaffected).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
    next_id: AtomicU64,
}

impl DesktopFilePicker {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Install a hook on `runtime` that removes the picker's cached
    /// path when the client drops the corresponding
    /// [`NativeHandle`](istmo_core::NativeHandle). Idempotent — installing
    /// twice registers two hooks, both fire per release; the second
    /// simply no-ops on already-gone entries.
    pub fn install_release_hook(&self, runtime: &Arc<Runtime>) {
        let weak = Arc::downgrade(&self.inner);
        runtime.install_native_handle_release_hook(Arc::new(move |id| {
            if let Some(inner) = weak.upgrade() {
                inner
                    .files
                    .lock()
                    .expect("files map")
                    .remove(&id.get());
            }
        }));
    }

    /// Drop the entire in-process URI/path map. Manual escape hatch
    /// when the release hook is not wired up.
    pub fn purge(&self) {
        self.inner.files.lock().expect("files map").clear();
    }

    fn alloc(&self, path: PathBuf) -> NativeHandleId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.inner
            .files
            .lock()
            .expect("files map")
            .insert(id, path);
        NativeHandleId::new(id)
    }

    fn alloc_many(&self, paths: Vec<PathBuf>) -> Vec<(PathBuf, NativeHandleId)> {
        paths
            .into_iter()
            .map(|p| {
                let id = self.alloc(p.clone());
                (p, id)
            })
            .collect()
    }

    fn lookup(&self, id: NativeHandleId) -> Result<PathBuf, FilePickerError> {
        self.inner
            .files
            .lock()
            .expect("files map")
            .get(&id.get())
            .cloned()
            .ok_or_else(|| FilePickerError::NotFound(format!("handle {}", id.get())))
    }
}

fn picked_from_path(path: &std::path::Path, handle: NativeHandleId) -> PickedFile {
    let display_name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let size = std::fs::metadata(path).ok().map(|m| m.len());
    PickedFile {
        display_name,
        mime_type: None,
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

fn apply_filter(mut d: rfd::FileDialog, filter: &FileFilter) -> rfd::FileDialog {
    if !filter.extensions.is_empty() {
        let exts: Vec<&str> = filter.extensions.iter().map(String::as_str).collect();
        d = d.add_filter("Filtered", &exts);
    }
    d
}

fn build_pick_dialog(config: &PickConfig) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if let Some(t) = &config.dialog_title {
        d = d.set_title(t);
    }
    if let Some(dir) = &config.start_directory_hint {
        d = d.set_directory(dir);
    }
    apply_filter(d, &config.filter)
}

fn build_save_dialog(config: &SaveConfig) -> rfd::FileDialog {
    let mut d = rfd::FileDialog::new();
    if let Some(t) = &config.dialog_title {
        d = d.set_title(t);
    }
    if let Some(dir) = &config.start_directory_hint {
        d = d.set_directory(dir);
    }
    if let Some(name) = &config.suggested_name {
        d = d.set_file_name(name);
    }
    apply_filter(d, &config.filter)
}

fn run_pick_single(config: &PickConfig) -> Option<PathBuf> {
    build_pick_dialog(config).pick_file()
}

fn run_pick_multi(config: &PickConfig) -> Vec<PathBuf> {
    build_pick_dialog(config).pick_files().unwrap_or_default()
}

fn run_save(config: &SaveConfig) -> Option<PathBuf> {
    build_save_dialog(config).save_file()
}

// ------------------------------------------------------------------ FilePicker impl

impl FilePicker for DesktopFilePicker {
    async fn pick_file(
        &self,
        config: PickConfig,
    ) -> Result<Option<PickedFile>, FilePickerError> {
        let Some(path) = run_pick_single(&config) else {
            return Ok(None);
        };
        let handle = self.alloc(path.clone());
        Ok(Some(picked_from_path(&path, handle)))
    }

    async fn pick_files(
        &self,
        config: PickConfig,
    ) -> Result<Vec<PickedFile>, FilePickerError> {
        let paths = run_pick_multi(&config);
        Ok(self
            .alloc_many(paths)
            .into_iter()
            .map(|(p, id)| picked_from_path(&p, id))
            .collect())
    }

    async fn save_file(
        &self,
        config: SaveConfig,
    ) -> Result<Option<PickedFile>, FilePickerError> {
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
        let handle = self.alloc(path.clone());
        Ok(Some(picked_from_path(&path, handle)))
    }

    async fn open_read(
        &self,
        file: NativeHandleId,
    ) -> Result<RawFileHandle, FilePickerError> {
        let path = self.lookup(file)?;
        let f = map_io(std::fs::File::open(&path), "open_read")?;
        Ok(RawFileHandle {
            raw: raw_from_file(f),
        })
    }

    async fn open_write(
        &self,
        file: NativeHandleId,
    ) -> Result<RawFileHandle, FilePickerError> {
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

    async fn path(
        &self,
        file: NativeHandleId,
    ) -> Result<Option<String>, FilePickerError> {
        let path = self.lookup(file)?;
        Ok(Some(path.to_string_lossy().into_owned()))
    }
}
