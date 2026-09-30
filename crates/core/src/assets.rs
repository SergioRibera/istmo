//! Read-only access to bundled assets.
//!
//! This module surfaces a Flutter-`assets:`-style API on top of the
//! per-platform bundle:
//!
//! - **Android** — assets read from the APK's `assets/` directory via
//!   `AAssetManager`.
//! - **iOS / bundled macOS** — assets read from the app's `NSBundle`
//!   Resources directory.
//! - **Desktop** — assets read from the filesystem, resolved via
//!   (1) the runtime `ISTMO_ASSETS_DIR` env var, (2) the build-time
//!   `ISTMO_APP_ASSETS_DIR` baked into the binary by `emit_app`,
//!   (3) `<exe_dir>/assets/`, and finally (4) `<CARGO_MANIFEST_DIR>/assets/`
//!   in dev builds.
//!
//! The public surface mirrors [`std::fs::read`] / [`std::fs::File`]
//! semantics: paths are [`Path`]s, results are [`io::Result`], and the
//! streaming reader implements [`Read`] + [`Seek`] so any code that
//! already accepts `impl Read + Seek` composes without adapter glue.
//!
//! ```no_run
//! use std::io::Read;
//! # fn main() -> std::io::Result<()> {
//! // Full read into memory:
//! let logo = istmo_core::assets::read("img/logo.png")?;
//! // Streaming read:
//! let mut reader = istmo_core::assets::open("data/schema.sql")?;
//! let mut sql = String::new();
//! reader.read_to_string(&mut sql)?;
//! // Plugin-scoped read:
//! let icon = istmo_core::assets::plugin("istmo.data_store").read("icon.svg")?;
//! # Ok(()) }
//! ```

use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::{Arc, OnceLock};

mod desktop;

/// The transport-neutral trait every platform backend implements.
///
/// Backends are installed exactly once per process via
/// [`__install_backend`]. Users never touch this trait directly — they
/// call the free functions [`read`] / [`open`] or the plugin-scoped
/// [`plugin`] helper.
pub trait AssetBackend: Send + Sync + 'static {
    /// Open the asset addressed by `key` inside the given `scope`.
    ///
    /// The returned reader must be `Read + Seek + Send` — mobile
    /// backends may wrap `AAsset` / `NSData` slices, desktop backends
    /// return a `std::fs::File`.
    fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader>;
}

/// Namespaces an asset lookup.
///
/// [`AssetScope::App`] resolves against the app's own `[[assets]]`
/// section; [`AssetScope::Plugin`] against the plugin crate that
/// advertised the same id in its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetScope<'a> {
    App,
    Plugin(&'a str),
}

/// Streaming reader returned by [`open`].
///
/// Impls [`Read`] and [`Seek`] so it slots into any function that
/// already accepts an `impl Read + Seek` — [`io::copy`],
/// [`io::BufReader::new`], serde readers, image decoders …
pub struct AssetReader {
    inner: Box<dyn ReadSeekSend>,
}

impl fmt::Debug for AssetReader {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AssetReader").finish_non_exhaustive()
    }
}

impl AssetReader {
    /// Wrap a boxed `Read + Seek + Send` value into an [`AssetReader`].
    /// Backends call this to hand their concrete reader shape back
    /// through the trait boundary.
    #[must_use]
    pub fn from_box<R: Read + Seek + Send + 'static>(inner: Box<R>) -> Self {
        Self { inner }
    }

    /// Wrap any owned reader into an [`AssetReader`].
    #[must_use]
    pub fn new<R: Read + Seek + Send + 'static>(inner: R) -> Self {
        Self {
            inner: Box::new(inner),
        }
    }
}

impl Read for AssetReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for AssetReader {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        self.inner.seek(pos)
    }
}

trait ReadSeekSend: Read + Seek + Send {}
impl<T: Read + Seek + Send> ReadSeekSend for T {}

/// Domain-level asset errors.
///
/// Consumers who want granular matching can downcast an [`io::Error`]
/// returned by [`read`] / [`open`] with [`io::Error::get_ref`].
#[derive(Debug)]
pub enum AssetError {
    /// The requested key does not resolve to a bundled asset.
    NotFound { scope: String, key: String },
    /// The platform backend has not been installed. Applies to Android
    /// only — desktop and iOS backends self-install on first use.
    BackendNotInstalled,
    /// Any lower-level I/O failure.
    Io(io::Error),
    /// Platform-specific failure the backend cannot map to [`AssetError::Io`].
    Backend(String),
}

impl fmt::Display for AssetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound { scope, key } => write!(f, "asset `{key}` not found in {scope}"),
            Self::BackendNotInstalled => {
                f.write_str("istmo asset backend has not been installed on this platform")
            }
            Self::Io(err) => write!(f, "asset io error: {err}"),
            Self::Backend(msg) => write!(f, "asset backend error: {msg}"),
        }
    }
}

impl std::error::Error for AssetError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for AssetError {
    fn from(value: io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<AssetError> for io::Error {
    fn from(value: AssetError) -> Self {
        match value {
            AssetError::Io(err) => err,
            AssetError::NotFound { scope, key } => io::Error::new(
                io::ErrorKind::NotFound,
                format!("asset `{key}` not found in {scope}"),
            ),
            AssetError::BackendNotInstalled => io::Error::new(
                io::ErrorKind::Unsupported,
                "istmo asset backend has not been installed on this platform",
            ),
            AssetError::Backend(msg) => io::Error::other(msg),
        }
    }
}

static BACKEND: OnceLock<Arc<dyn AssetBackend>> = OnceLock::new();

/// Install the process-wide asset backend.
///
/// Called exactly once — usually from the platform transport crate
/// (`istmo-android::nativeInstallContext`, `istmo-ios::istmo_ios_start`).
/// The second and subsequent calls are ignored so the initial choice
/// wins. Tests can install a mock backend before the first
/// [`read`] / [`open`].
pub fn install_backend(backend: Arc<dyn AssetBackend>) {
    let _ = BACKEND.set(backend);
}

fn backend() -> Arc<dyn AssetBackend> {
    BACKEND
        .get_or_init(|| Arc::new(desktop::DesktopAssetBackend::new()))
        .clone()
}

/// Read a whole app-owned asset into memory.
///
/// # Errors
///
/// Returns the platform backend's [`io::Error`] on lookup failure —
/// including [`AssetError::NotFound`] mapped to
/// [`io::ErrorKind::NotFound`].
pub fn read(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    read_inner(AssetScope::App, path.as_ref())
}

/// Open a streaming reader for an app-owned asset.
///
/// # Errors
///
/// Returns the platform backend's [`io::Error`] on lookup failure.
pub fn open(path: impl AsRef<Path>) -> io::Result<AssetReader> {
    open_inner(AssetScope::App, path.as_ref())
}

/// Plugin-scoped asset accessor.
#[must_use]
pub fn plugin(id: &'static str) -> PluginAssets {
    PluginAssets { id }
}

/// Handle returned by [`plugin`] for plugin-scoped asset reads.
#[derive(Debug, Clone, Copy)]
pub struct PluginAssets {
    id: &'static str,
}

impl PluginAssets {
    /// Full read of a plugin-owned asset.
    ///
    /// # Errors
    ///
    /// Returns the platform backend's [`io::Error`] on lookup failure.
    pub fn read(&self, path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
        read_inner(AssetScope::Plugin(self.id), path.as_ref())
    }

    /// Streaming read of a plugin-owned asset.
    ///
    /// # Errors
    ///
    /// Returns the platform backend's [`io::Error`] on lookup failure.
    pub fn open(&self, path: impl AsRef<Path>) -> io::Result<AssetReader> {
        open_inner(AssetScope::Plugin(self.id), path.as_ref())
    }
}

fn read_inner(scope: AssetScope<'_>, path: &Path) -> io::Result<Vec<u8>> {
    let key = normalise_key(path)?;
    let mut reader = backend().open(scope, &key)?;
    let mut buf = Vec::new();
    reader.read_to_end(&mut buf)?;
    Ok(buf)
}

fn open_inner(scope: AssetScope<'_>, path: &Path) -> io::Result<AssetReader> {
    let key = normalise_key(path)?;
    backend().open(scope, &key)
}

/// Turn any [`Path`] into the platform-neutral forward-slash bundle key
/// backends expect.
///
/// - Absolute paths and paths containing `..` are rejected — a bundle
///   key must always resolve *inside* the crate root that declared it.
/// - Backslashes are normalised to forward slashes so callers can pass
///   Windows-style paths in tests.
/// - Empty keys are rejected.
fn normalise_key(path: &Path) -> io::Result<String> {
    if path.as_os_str().is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "asset key is empty",
        ));
    }
    if path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("asset key must be relative: `{}`", path.display()),
        ));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        use std::path::Component::{CurDir, Normal, ParentDir, Prefix, RootDir};
        match component {
            Normal(part) => parts.push(part.to_string_lossy().into_owned()),
            CurDir => continue,
            ParentDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("asset key must not traverse parents: `{}`", path.display()),
                ));
            }
            Prefix(_) | RootDir => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("asset key must be relative: `{}`", path.display()),
                ));
            }
        }
    }
    if parts.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "asset key is empty",
        ));
    }
    Ok(parts.join("/"))
}

#[doc(hidden)]
pub use desktop::DesktopAssetBackend;
#[doc(hidden)]
pub use desktop::set_app_assets;

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Cursor;
    use std::sync::Mutex;

    struct MockBackend {
        entries: Mutex<Vec<(String, String, Vec<u8>)>>,
    }

    impl AssetBackend for MockBackend {
        fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader> {
            let scope_str = match scope {
                AssetScope::App => "app".to_string(),
                AssetScope::Plugin(id) => format!("plugin:{id}"),
            };
            let entries = self.entries.lock().unwrap();
            let bytes = entries
                .iter()
                .find(|(s, k, _)| *s == scope_str && *k == key)
                .map(|(_, _, b)| b.clone())
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("miss {key}")))?;
            Ok(AssetReader::new(Cursor::new(bytes)))
        }
    }

    #[test]
    fn normalise_rejects_empty() {
        assert!(normalise_key(Path::new("")).is_err());
    }

    #[test]
    fn normalise_rejects_absolute() {
        assert!(normalise_key(Path::new("/etc/hosts")).is_err());
    }

    #[test]
    fn normalise_rejects_parent_dir() {
        assert!(normalise_key(Path::new("../secret")).is_err());
    }

    #[test]
    fn normalise_forward_slashes() {
        let key = normalise_key(Path::new("img/icons/gear.png")).unwrap();
        assert_eq!(key, "img/icons/gear.png");
    }

    #[test]
    fn read_all_returns_full_bytes() {
        // Install a mock backend for tests. This will silently no-op on
        // subsequent installs, so all tests in this module share the
        // backend — they must not collide on keys.
        let backend = Arc::new(MockBackend {
            entries: Mutex::new(vec![
                ("app".into(), "hello.txt".into(), b"hi".to_vec()),
                (
                    "plugin:istmo.demo".into(),
                    "icon.svg".into(),
                    b"<svg/>".to_vec(),
                ),
            ]),
        });
        install_backend(backend);
        let hello = read("hello.txt").expect("read");
        assert_eq!(hello, b"hi");
        let icon = plugin("istmo.demo").read("icon.svg").expect("read");
        assert_eq!(icon, b"<svg/>");
    }

    #[test]
    fn desktop_backend_reads_from_env_dir() {
        let tmp = std::env::temp_dir().join("istmo-assets-desktop-env");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("plugin:istmo.example")).unwrap();
        fs::write(tmp.join("root.txt"), b"root").unwrap();
        fs::write(tmp.join("plugin:istmo.example/deep.txt"), b"deep").unwrap();
        // Exercise the desktop backend end-to-end without installing it
        // globally (which would race with `read_all_returns_full_bytes`
        // that already installed a mock).
        let backend = desktop::DesktopAssetBackend::for_root(tmp.clone());
        let mut reader = backend.open(AssetScope::App, "root.txt").unwrap();
        let mut buf = Vec::new();
        reader.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"root");
        let mut plugin_reader = backend
            .open(AssetScope::Plugin("istmo.example"), "deep.txt")
            .unwrap();
        let mut buf2 = Vec::new();
        plugin_reader.read_to_end(&mut buf2).unwrap();
        assert_eq!(buf2, b"deep");
    }
}
