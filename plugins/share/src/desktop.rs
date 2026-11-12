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

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::task::Poll;
use std::time::{Duration, SystemTime};

use istmo_window::{NativeWindow, WindowId, WindowRegistry};

use istmo_core::CancelToken;
use istmo_core::sync::lock;

use crate::{
    Share, ShareCapabilities, ShareError, ShareFile, ShareFileSource, ShareOutcome, ShareRequest,
    ShareTarget, sanitize_file_name,
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
pub use macos::{drain_app_group_inbox, observe_app_group_handoffs};

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
    async fn share(
        &self,
        cancel: CancelToken,
        request: ShareRequest,
    ) -> Result<ShareOutcome, ShareError> {
        request.validate()?;
        if !platform::capabilities().send {
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
        self.inner.platform.present(job, &cancel).await
    }

    async fn capabilities(&self) -> Result<ShareCapabilities, ShareError> {
        Ok(platform::capabilities())
    }

    async fn staging_dir(&self) -> Result<String, ShareError> {
        Ok(self.inner.staging.root()?.display().to_string())
    }

    async fn set_share_targets(&self, _targets: Vec<ShareTarget>) -> Result<(), ShareError> {
        Err(ShareError::Unsupported(
            "desktop share sheets have no direct-share row".to_owned(),
        ))
    }
}

/// Run `fut` until it finishes or `cancel` fires, whichever comes
/// first. `None` means cancelled.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) async fn until_cancelled<F: Future>(cancel: &CancelToken, fut: F) -> Option<F::Output> {
    let mut fut = std::pin::pin!(fut);
    let mut cancelled = std::pin::pin!(cancel.cancelled());
    std::future::poll_fn(|cx| {
        if let Poll::Ready(output) = fut.as_mut().poll(cx) {
            return Poll::Ready(Some(output));
        }
        cancelled.as_mut().poll(cx).map(|()| None)
    })
    .await
}

/// Delete entries of `dir` last modified more than `max_age` ago.
/// Received files the app never cleaned up go this way.
#[cfg_attr(target_os = "linux", allow(dead_code))]
pub(crate) fn prune_stale_entries(dir: &Path, max_age: Duration) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.filter_map(Result::ok) {
        let stale = entry
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age > max_age);
        if !stale {
            continue;
        }
        let path = entry.path();
        let removed = if path.is_dir() {
            std::fs::remove_dir_all(&path)
        } else {
            std::fs::remove_file(&path)
        };
        if let Err(err) = removed {
            tracing::warn!(?err, entry = %path.display(), "pruning stale share entry");
        }
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

/// Prefix of every staging directory name.
const STAGING_PREFIX: &str = "istmo-share-";

/// Staging directories older than this belong to a process that died
/// without cleaning up.
const STAGING_TTL: Duration = Duration::from_secs(24 * 60 * 60);

/// Private directory holding [`ShareFileSource::Bytes`] payloads.
///
/// The directory is created on first use under a fresh, unguessable
/// name with `create_dir` — which refuses an existing path, so nothing
/// another local user planted there (a symlink, a pre-made directory)
/// is ever reused — and owner-only permissions (`0o700`, files
/// `0o600`). It lives under `$XDG_RUNTIME_DIR/istmo-share/` when that
/// is a directory only this user can write to, otherwise directly in the
/// sticky temp directory.
/// [`Staging::purge`] and drop remove it; directories a crashed process
/// left behind are removed after [`STAGING_TTL`] when the next backend
/// starts.
#[derive(Debug)]
struct Staging {
    parent: PathBuf,
    root: Mutex<Option<PathBuf>>,
    next: AtomicU64,
}

impl Default for Staging {
    fn default() -> Self {
        let parent = Self::parent();
        purge_stale(&parent, STAGING_TTL);
        let temp = std::env::temp_dir();
        if parent != temp {
            // Where staging falls back to when `parent` is not private.
            purge_stale(&temp, STAGING_TTL);
        }
        Self {
            parent,
            root: Mutex::new(None),
            next: AtomicU64::new(0),
        }
    }
}

impl Staging {
    fn parent() -> PathBuf {
        std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .filter(|dir| dir.is_absolute())
            .map_or_else(std::env::temp_dir, |dir| dir.join("istmo-share"))
    }

    /// The staging directory, created on first use.
    #[expect(
        clippy::significant_drop_tightening,
        reason = "the lock is held across creation so concurrent stages share one directory"
    )]
    fn root(&self) -> Result<PathBuf, ShareError> {
        let mut root = lock(&self.root);
        if let Some(root) = root.as_ref() {
            return Ok(root.clone());
        }
        let temp = std::env::temp_dir();
        let mut parent = self.parent.as_path();
        if parent != temp {
            private_dir_builder()
                .recursive(true)
                .create(parent)
                .map_err(|e| io_error(parent, &e))?;
            // `recursive` accepts whatever already sits at that path. A
            // directory someone else controls could have the staging dir
            // renamed and swapped under it; the sticky temp dir cannot.
            if !is_private_dir(parent) {
                tracing::warn!(dir = %parent.display(), "share staging parent is not private; using the temp dir");
                parent = temp.as_path();
            }
        }
        let created = create_unique_dir(parent)?;
        *root = Some(created.clone());
        Ok(created)
    }

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
                // One directory per file keeps the receiver-visible name
                // intact even when two files share it.
                let dir = self
                    .root()?
                    .join(self.next.fetch_add(1, Ordering::Relaxed).to_string());
                private_dir_builder()
                    .create(&dir)
                    .map_err(|e| io_error(&dir, &e))?;
                let path = dir.join(name);
                write_private(&path, bytes).map_err(|e| io_error(&path, &e))?;
                path
            }
        };
        Ok(StagedFile { path, mime_type })
    }

    fn purge(&self) {
        let Some(root) = lock(&self.root).take() else {
            return;
        };
        if let Err(err) = std::fs::remove_dir_all(&root)
            && err.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(?err, root = %root.display(), "purging staged share files");
        }
    }
}

impl Drop for Staging {
    fn drop(&mut self) {
        self.purge();
    }
}

fn private_dir_builder() -> std::fs::DirBuilder {
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// Create `<parent>/istmo-share-<random>`, retrying on the (unlikely)
/// collision with an existing entry.
fn create_unique_dir(parent: &Path) -> Result<PathBuf, ShareError> {
    use std::hash::{BuildHasher, Hasher};

    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let mut last_err = None;
    for _ in 0..16 {
        // `RandomState` is seeded from OS randomness; mixing in the pid,
        // a counter and the clock keeps names distinct even if it is not.
        let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
        hasher.write_u32(std::process::id());
        hasher.write_u64(COUNTER.fetch_add(1, Ordering::Relaxed));
        hasher.write_u128(
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos()),
        );
        let dir = parent.join(format!("{STAGING_PREFIX}{:016x}", hasher.finish()));
        match private_dir_builder().create(&dir) {
            Ok(()) => return Ok(dir),
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => last_err = Some(err),
            Err(err) => return Err(io_error(&dir, &err)),
        }
    }
    let err = last_err.unwrap_or_else(|| std::io::Error::other("no attempt made"));
    Err(io_error(parent, &err))
}

/// Write `bytes` to a new owner-only file; an existing path is an error.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)?.write_all(bytes)
}

/// Remove staging directories under `parent` not modified for `ttl`.
/// Only real directories carrying [`STAGING_PREFIX`] (and, on Unix,
/// owned by this user) are touched; symlinks are never followed.
fn purge_stale(parent: &Path, ttl: Duration) {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return;
    };
    let now = SystemTime::now();
    for entry in entries.filter_map(Result::ok) {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| name.starts_with(STAGING_PREFIX))
        {
            continue;
        }
        let path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if !meta.is_dir() || !owned_by_current_user(&meta) {
            continue;
        }
        let stale = meta
            .modified()
            .ok()
            .and_then(|modified| now.duration_since(modified).ok())
            .is_some_and(|age| age >= ttl);
        if stale && let Err(err) = std::fs::remove_dir_all(&path) {
            tracing::debug!(?err, dir = %path.display(), "removing stale share staging dir");
        }
    }
}

#[cfg(unix)]
fn owned_by_current_user(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;

    // Without `libc` there is no `getuid`; the owner of a file this
    // process creates is the effective uid.
    static UID: std::sync::OnceLock<Option<u32>> = std::sync::OnceLock::new();
    let uid = UID.get_or_init(|| {
        let probe =
            std::env::temp_dir().join(format!("{STAGING_PREFIX}uid-probe-{}", std::process::id()));
        // `create_new` never follows a symlink planted under that name.
        let uid = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&probe)
            .and_then(|file| file.metadata())
            .map(|probe_meta| probe_meta.uid())
            .ok();
        drop(std::fs::remove_file(&probe));
        uid
    });
    uid.is_some_and(|uid| uid == meta.uid())
}

/// Whether `dir` and the directory holding it are real directories of
/// this user that nobody else can write to.
#[cfg(unix)]
fn is_private_dir(dir: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    let private = |path: &Path| {
        std::fs::symlink_metadata(path).is_ok_and(|meta| {
            meta.is_dir() && owned_by_current_user(&meta) && meta.permissions().mode() & 0o022 == 0
        })
    };
    // The holder (`$XDG_RUNTIME_DIR`) may itself be reached through a
    // symlink; what matters is the directory it resolves to.
    let holder_private = dir
        .parent()
        .and_then(|holder| std::fs::canonicalize(holder).ok())
        .is_some_and(|holder| private(&holder));
    holder_private && private(dir)
}

#[cfg(not(unix))]
const fn is_private_dir(_dir: &Path) -> bool {
    // Only Unix reads `XDG_RUNTIME_DIR`; elsewhere the parent is the
    // per-user temp directory and never reaches this check.
    true
}

#[cfg(not(unix))]
const fn owned_by_current_user(_meta: &std::fs::Metadata) -> bool {
    // The Windows temp directory is already per user.
    true
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
        let was_busy = std::mem::replace(&mut *lock(flag), true);
        if was_busy {
            return Err(ShareError::Busy);
        }
        Ok(Self { flag })
    }
}

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        *lock(self.flag) = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stages_bytes_and_purges() {
        let staging = Staging::default();
        let staged = staging
            .stage(&ShareFile::bytes("note.txt", b"hi".to_vec()))
            .expect("stage");
        assert_eq!(std::fs::read(&staged.path).expect("read"), b"hi");
        assert_eq!(staged.mime_type, "text/plain");
        let root = staging.root().expect("root");
        assert!(staged.path.starts_with(&root));
        assert!(
            root.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(STAGING_PREFIX))
        );
        staging.purge();
        assert!(!staged.path.exists());
        assert!(!root.exists());
        // A purged staging area starts over in a fresh directory.
        let again = staging
            .stage(&ShareFile::bytes("note.txt", b"hi".to_vec()))
            .expect("stage again");
        assert!(!again.path.starts_with(&root));
    }

    #[cfg(unix)]
    #[test]
    fn staged_files_are_private() {
        use std::os::unix::fs::PermissionsExt;

        let staging = Staging::default();
        let staged = staging
            .stage(&ShareFile::bytes("secret.txt", b"x".to_vec()))
            .expect("stage");
        let mode = |path: &Path| {
            std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode()
                & 0o777
        };
        assert_eq!(mode(&staging.root().expect("root")), 0o700);
        assert_eq!(mode(staged.path.parent().expect("dir")), 0o700);
        assert_eq!(mode(&staged.path), 0o600);
    }

    #[test]
    fn purges_stale_staging_dirs_only() {
        let parent =
            std::env::temp_dir().join(format!("istmo-share-test-parent-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&parent));
        std::fs::create_dir_all(&parent).expect("parent");
        let stale = parent.join(format!("{STAGING_PREFIX}stale"));
        let fresh = parent.join(format!("{STAGING_PREFIX}fresh"));
        let foreign = parent.join("unrelated");
        for dir in [&stale, &fresh, &foreign] {
            std::fs::create_dir(dir).expect("dir");
        }
        let old = SystemTime::now() - Duration::from_secs(3 * 24 * 60 * 60);
        for dir in [&stale, &foreign] {
            std::fs::File::open(dir)
                .expect("open dir")
                .set_modified(old)
                .expect("backdate");
        }

        purge_stale(&parent, STAGING_TTL);

        assert!(!stale.exists(), "stale staging dir removed");
        assert!(fresh.exists(), "fresh staging dir kept");
        assert!(
            foreign.exists(),
            "dirs without the prefix are never touched"
        );
        drop(std::fs::remove_dir_all(&parent));
    }

    #[cfg(unix)]
    #[test]
    fn writable_staging_parents_are_not_private() {
        use std::os::unix::fs::PermissionsExt;

        let holder =
            std::env::temp_dir().join(format!("istmo-share-test-holder-{}", std::process::id()));
        drop(std::fs::remove_dir_all(&holder));
        private_dir_builder().create(&holder).expect("holder");
        let dir = holder.join("istmo-share");
        private_dir_builder().create(&dir).expect("dir");
        assert!(is_private_dir(&dir));

        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert!(!is_private_dir(&dir), "world-writable dir");
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o777)).expect("chmod");
        assert!(!is_private_dir(&dir), "world-writable holder");
        std::fs::set_permissions(&holder, std::fs::Permissions::from_mode(0o700)).expect("chmod");

        let link = holder.join("link");
        std::os::unix::fs::symlink(&dir, &link).expect("symlink");
        assert!(!is_private_dir(&link), "symlink");

        drop(std::fs::remove_dir_all(&holder));
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
