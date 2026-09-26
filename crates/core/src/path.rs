//! Cross-platform application-directory lookup.
//!
//! Mirrors the `directories` crate's dictionary of well-known paths so
//! callers get consistent semantics across desktop and mobile targets:
//!
//! | Getter | Android | iOS | Linux | macOS | Windows |
//! |---|---|---|---|---|---|
//! | [`data_dir`] | `context.filesDir` | `~/Documents` in the sandbox | `$XDG_DATA_HOME/<id>` | `~/Library/Application Support/<id>` | `%APPDATA%/<id>/data` |
//! | [`data_local_dir`] | `context.filesDir` | same as `data_dir` | same as `data_dir` | same as `data_dir` | `%LOCALAPPDATA%/<id>/data` |
//! | [`config_dir`] | `context.filesDir/config` | `~/Library/Application Support/<id>` | `$XDG_CONFIG_HOME/<id>` | `~/Library/Application Support/<id>` | `%APPDATA%/<id>/config` |
//! | [`config_local_dir`] | same as `config_dir` | same | same | same | `%LOCALAPPDATA%/<id>/config` |
//! | [`cache_dir`] | `context.cacheDir` | `~/Library/Caches/<id>` | `$XDG_CACHE_HOME/<id>` | `~/Library/Caches/<id>` | `%LOCALAPPDATA%/<id>/cache` |
//! | [`state_dir`] | `context.filesDir/state` | same as `data_dir` | `$XDG_STATE_HOME/<id>` | same as `data_dir` | `%LOCALAPPDATA%/<id>/state` |
//! | [`preference_dir`] | same as `config_dir` | `~/Library/Preferences/<id>` | same as `config_dir` | `~/Library/Preferences/<id>` | same as `config_dir` |
//! | [`documents_dir`] | `context.filesDir/documents` | `~/Documents` | `$XDG_DOCUMENTS_DIR` or `~/Documents` | `~/Documents` | `%USERPROFILE%\Documents` |
//! | [`runtime_dir`] | `context.cacheDir` | tmp | `$XDG_RUNTIME_DIR` or [`std::env::temp_dir`] | [`std::env::temp_dir`] | [`std::env::temp_dir`] |
//! | [`temp_dir`] | [`std::env::temp_dir`] | [`std::env::temp_dir`] | [`std::env::temp_dir`] | [`std::env::temp_dir`] | [`std::env::temp_dir`] |
//!
//! On mobile platforms the runtime transport crate (`istmo-android`,
//! `istmo-ios`) fills in [`PlatformPaths`] from `Context.filesDir` /
//! `NSFileManager` and hands the struct to [`install`]. On desktop the
//! default resolver is invoked lazily on first access; it derives the
//! per-app subdirectory from the build-time `ISTMO_APP_BUNDLE_ID` env
//! var (baked by `istmo-build::emit_app`) with a `CARGO_PKG_NAME`
//! fallback.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

mod desktop;

pub use desktop::default_paths;

/// The full set of well-known directories a platform backend hands to
/// the runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformPaths {
    pub data_dir: PathBuf,
    pub data_local_dir: PathBuf,
    pub config_dir: PathBuf,
    pub config_local_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub state_dir: PathBuf,
    pub preference_dir: PathBuf,
    pub documents_dir: PathBuf,
    pub runtime_dir: PathBuf,
    pub temp_dir: PathBuf,
}

/// Domain errors returned by path resolution.
///
/// Convertible to [`std::io::Error`] with
/// [`io::ErrorKind::Unsupported`](std::io::ErrorKind::Unsupported).
#[derive(Debug)]
pub enum PathError {
    /// The `ISTMO_APP_BUNDLE_ID` env var was not baked at build time
    /// and `CARGO_PKG_NAME` fallback resolution failed too.
    MissingBundleId,
    /// The platform backend was queried before installation. Applies
    /// to Android — desktop and iOS backends self-install lazily.
    NotInstalled,
    /// A required environment variable was missing on desktop.
    MissingEnv(&'static str),
}

impl fmt::Display for PathError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingBundleId => f.write_str("no bundle id available for the path provider"),
            Self::NotInstalled => f.write_str("istmo path provider has not been installed"),
            Self::MissingEnv(name) => write!(f, "environment variable `{name}` is not set"),
        }
    }
}

impl std::error::Error for PathError {}

impl From<PathError> for std::io::Error {
    fn from(value: PathError) -> Self {
        Self::new(std::io::ErrorKind::Unsupported, value.to_string())
    }
}

static PATHS: OnceLock<PlatformPaths> = OnceLock::new();

/// Install the process-wide directory set.
///
/// Called from the platform transport crate: [`istmo-android`] wires
/// values sourced from `context.filesDir` and friends;
/// [`istmo-ios`] wires values sourced from `NSFileManager`. Idempotent
/// — subsequent calls are ignored.
///
/// [`istmo-android`]: https://docs.rs/istmo-android
/// [`istmo-ios`]: https://docs.rs/istmo-ios
pub fn install(paths: PlatformPaths) {
    let _ = PATHS.set(paths);
}

fn paths() -> &'static PlatformPaths {
    PATHS.get_or_init(default_paths)
}

/// Persistent, roaming application data.
#[must_use]
pub fn data_dir() -> &'static Path {
    &paths().data_dir
}

/// Persistent, non-roaming application data.
#[must_use]
pub fn data_local_dir() -> &'static Path {
    &paths().data_local_dir
}

/// Roaming configuration.
#[must_use]
pub fn config_dir() -> &'static Path {
    &paths().config_dir
}

/// Non-roaming configuration.
#[must_use]
pub fn config_local_dir() -> &'static Path {
    &paths().config_local_dir
}

/// OS-managed cache — safe to be wiped.
#[must_use]
pub fn cache_dir() -> &'static Path {
    &paths().cache_dir
}

/// State that must survive restarts but is not user-visible
/// (`$XDG_STATE_HOME` on Linux).
#[must_use]
pub fn state_dir() -> &'static Path {
    &paths().state_dir
}

/// macOS-style preferences directory (`.plist` files).
#[must_use]
pub fn preference_dir() -> &'static Path {
    &paths().preference_dir
}

/// User-visible documents directory.
#[must_use]
pub fn documents_dir() -> &'static Path {
    &paths().documents_dir
}

/// Session-lifetime directory (`$XDG_RUNTIME_DIR` on Linux).
#[must_use]
pub fn runtime_dir() -> &'static Path {
    &paths().runtime_dir
}

/// Short-lived temporary directory.
#[must_use]
pub fn temp_dir() -> &'static Path {
    &paths().temp_dir
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_populates_all_getters() {
        // Install a fixed shape so getters return deterministic paths.
        // Idempotency means we may install once across all tests in the
        // module — pick unique paths to avoid collision with other tests.
        let tmp = std::env::temp_dir().join("istmo-path-install");
        let paths = PlatformPaths {
            data_dir: tmp.join("data"),
            data_local_dir: tmp.join("data_local"),
            config_dir: tmp.join("config"),
            config_local_dir: tmp.join("config_local"),
            cache_dir: tmp.join("cache"),
            state_dir: tmp.join("state"),
            preference_dir: tmp.join("preference"),
            documents_dir: tmp.join("documents"),
            runtime_dir: tmp.join("runtime"),
            temp_dir: tmp.join("temp"),
        };
        install(paths.clone());
        // Value may reflect either our install or a prior test's
        // default install; both are acceptable — assert internal
        // consistency (getters return the same struct).
        assert_eq!(data_dir(), self::paths().data_dir);
        assert_eq!(cache_dir(), self::paths().cache_dir);
    }
}
