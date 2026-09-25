//! Rust-hosted desktop backend.
//!
//! * **Windows** — `DataTransferManager` through
//!   `IDataTransferManagerInterop`, anchored on the window's `HWND`.
//!   The share UI must be driven from the thread that owns the window,
//!   so each registered window is subclassed and share jobs are posted
//!   to it as window messages.
//! * **macOS** — `NSSharingServicePicker` anchored on the window's
//!   `NSView`, driven on the main queue.
//! * **Linux** — no standard share sheet exists (freedesktop has no
//!   share portal); [`Share::share`] returns [`ShareError::Unsupported`]
//!   and [`Share::capabilities`] reports everything `false` so apps can
//!   render their own targets.
//!
//! Windows are resolved through an [`istmo_window::WindowRegistry`]
//! (the process-global one by default), the same registry other
//! window-aware plugins use.
//!
//! ```ignore
//! let registry = istmo_window::WindowRegistry::global();
//! registry.register(WindowId(1), &window)?;
//! let backend = DesktopShare::new(Arc::clone(registry))?;
//! let init = Runtime::mock()
//!     .expects::<ShareClient>()
//!     .host(ShareHost::new(backend))
//!     .finish();
//! ```

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use istmo_window::{NativeWindow, WindowId, WindowRegistry};

use crate::{
    Share, ShareCapabilities, ShareError, ShareFile, ShareFileSource, ShareOutcome, ShareRequest,
};

#[cfg(any(target_os = "macos", test))]
mod handoff;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
use linux as platform;
#[cfg(target_os = "macos")]
use macos as platform;
#[cfg(target_os = "windows")]
use windows as platform;

#[cfg(target_os = "windows")]
pub use windows::take_share_target_activation;

#[cfg(target_os = "macos")]
pub use macos::drain_app_group_inbox;

/// Desktop [`Share`] backend. Cheap to clone; clones share state.
#[derive(Debug, Clone)]
pub struct DesktopShare {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    registry: Arc<WindowRegistry>,
    staging: Staging,
    platform: platform::Backend,
    /// Serialises share sheets: every platform tolerates one at a time.
    busy: Mutex<bool>,
}

impl DesktopShare {
    /// Backend resolving windows through `registry`.
    ///
    /// On Windows this subscribes to the registry so every window
    /// (already registered or registered later) gets the message hook
    /// the share UI needs; register windows from their UI thread.
    pub fn new(registry: Arc<WindowRegistry>) -> Result<Self, ShareError> {
        let platform = platform::Backend::attach(&registry)?;
        Ok(Self {
            inner: Arc::new(Inner {
                registry,
                staging: Staging::default(),
                platform,
                busy: Mutex::new(false),
            }),
        })
    }

    /// Backend using [`WindowRegistry::global`].
    pub fn with_global_registry() -> Result<Self, ShareError> {
        Self::new(Arc::clone(WindowRegistry::global()))
    }

    /// Delete files staged for earlier shares. Receivers may read
    /// shared files after [`Share::share`] resolves (Windows targets
    /// read them lazily), so staged files otherwise live until the
    /// backend drops.
    pub fn purge_staged(&self) {
        self.inner.staging.purge();
    }

    fn resolve_window(
        &self,
        request: &ShareRequest,
    ) -> Result<(WindowId, NativeWindow), ShareError> {
        let requested = request.anchor.and_then(|a| a.window_id).map(WindowId);
        let found = requested.map_or_else(
            || self.inner.registry.any_attached(),
            |id| self.inner.registry.get(id).map(|w| (id, w)),
        );
        found.ok_or_else(|| {
            ShareError::NoPresenter(requested.map_or_else(
                || "no window registered in the istmo-window registry".to_owned(),
                |id| format!("{id} is not registered in the istmo-window registry"),
            ))
        })
    }
}

impl Share for DesktopShare {
    async fn share(&self, request: ShareRequest) -> Result<ShareOutcome, ShareError> {
        request.validate()?;
        if !platform::CAPABILITIES.send {
            return Err(ShareError::Unsupported(
                platform::UNSUPPORTED_REASON.to_owned(),
            ));
        }
        let _guard = BusyGuard::acquire(&self.inner.busy)?;
        let window = self.resolve_window(&request)?;
        let files = self.inner.staging.stage_all(&request.files)?;
        let thumbnail = match request.preview.as_ref().and_then(|p| p.thumbnail.as_ref()) {
            Some(file) => Some(self.inner.staging.stage(file)?),
            None => None,
        };
        let job = ShareJob {
            request,
            files,
            thumbnail,
            window,
        };
        self.inner.platform.present(job).await
    }

    async fn capabilities(&self) -> Result<ShareCapabilities, ShareError> {
        Ok(platform::CAPABILITIES)
    }
}

/// A validated request with every file resolved to a real path.
#[derive(Debug)]
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
pub(crate) struct ShareJob {
    pub(crate) request: ShareRequest,
    pub(crate) files: Vec<StagedFile>,
    pub(crate) thumbnail: Option<StagedFile>,
    pub(crate) window: (WindowId, NativeWindow),
}

/// A file ready to hand to the OS.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) struct StagedFile {
    pub(crate) path: PathBuf,
    pub(crate) mime_type: String,
}

/// Temporary directory holding [`ShareFileSource::Bytes`] payloads.
#[derive(Debug)]
struct Staging {
    root: PathBuf,
    next: AtomicU64,
}

impl Default for Staging {
    fn default() -> Self {
        static INSTANCE: AtomicU64 = AtomicU64::new(0);
        let instance = INSTANCE.fetch_add(1, Ordering::Relaxed);
        Self {
            root: std::env::temp_dir()
                .join("istmo-share")
                .join(format!("{}-{instance}", std::process::id())),
            next: AtomicU64::new(0),
        }
    }
}

impl Staging {
    fn stage_all(&self, files: &[ShareFile]) -> Result<Vec<StagedFile>, ShareError> {
        files.iter().map(|file| self.stage(file)).collect()
    }

    fn stage(&self, file: &ShareFile) -> Result<StagedFile, ShareError> {
        let mime_type = file.effective_mime_type().to_owned();
        let path = match &file.source {
            ShareFileSource::Path(path) => {
                let path = PathBuf::from(path);
                if !path.is_file() {
                    return Err(ShareError::NotFound(path.display().to_string()));
                }
                path
            }
            ShareFileSource::Bytes(bytes) => {
                let name = sanitize_file_name(file.display_name().unwrap_or("shared"));
                let dir = self
                    .root
                    .join(self.next.fetch_add(1, Ordering::Relaxed).to_string());
                std::fs::create_dir_all(&dir).map_err(|e| io_error(&dir, &e))?;
                let path = dir.join(name);
                std::fs::write(&path, bytes).map_err(|e| io_error(&path, &e))?;
                path
            }
        };
        Ok(StagedFile { path, mime_type })
    }

    fn purge(&self) {
        if let Err(err) = std::fs::remove_dir_all(&self.root) {
            if err.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(?err, root = %self.root.display(), "purging staged share files");
            }
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        self.purge();
    }
}

/// Keep only the final path component and drop characters Windows
/// rejects, so a hostile or sloppy name cannot escape the staging dir.
fn sanitize_file_name(name: &str) -> String {
    let base = Path::new(name)
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("shared");
    let cleaned: String = base
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    if cleaned.trim_matches('.').is_empty() {
        "shared".to_owned()
    } else {
        cleaned
    }
}

fn io_error(path: &Path, err: &std::io::Error) -> ShareError {
    ShareError::Io(format!("{}: {err}", path.display()))
}

/// Holds the "a sheet is open" flag for the duration of a share.
struct BusyGuard<'a> {
    flag: &'a Mutex<bool>,
}

impl<'a> BusyGuard<'a> {
    fn acquire(flag: &'a Mutex<bool>) -> Result<Self, ShareError> {
        let was_busy = std::mem::replace(
            &mut *flag
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
            true,
        );
        if was_busy {
            return Err(ShareError::Busy);
        }
        Ok(Self { flag })
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *self
            .flag
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_hostile_names() {
        assert_eq!(sanitize_file_name("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_file_name("a:b*c?.txt"), "a_b_c_.txt");
        assert_eq!(sanitize_file_name(".."), "shared");
        assert_eq!(sanitize_file_name(""), "shared");
    }

    #[test]
    fn stages_bytes_and_purges() {
        let staging = Staging::default();
        let staged = staging
            .stage(&ShareFile::bytes("note.txt", b"hi".to_vec()))
            .expect("stage");
        assert_eq!(std::fs::read(&staged.path).expect("read"), b"hi");
        assert_eq!(staged.mime_type, "text/plain");
        staging.purge();
        assert!(!staged.path.exists());
    }

    #[test]
    fn missing_path_is_not_found() {
        let staging = Staging::default();
        let err = staging
            .stage(&ShareFile::path("/definitely/not/here.png"))
            .expect_err("missing");
        assert!(matches!(err, ShareError::NotFound(_)));
    }

    #[test]
    fn busy_guard_rejects_reentry() {
        let flag = Mutex::new(false);
        let first = BusyGuard::acquire(&flag).expect("first");
        assert!(matches!(BusyGuard::acquire(&flag), Err(ShareError::Busy)));
        drop(first);
        assert!(BusyGuard::acquire(&flag).is_ok());
    }
}
