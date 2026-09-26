//! Default desktop directory resolution.
//!
//! Follows the XDG Base Directory Specification on Linux, Apple's
//! [File System Basics] on macOS, and the Windows Known Folders IDs on
//! Windows — the same layout the `directories` crate produces, without
//! the crate dependency. All Windows lookups go through environment
//! variables (`%APPDATA%`, `%LOCALAPPDATA%`, `%USERPROFILE%`); the
//! Known Folders C API is not called since that would require
//! `winapi`.
//!
//! [File System Basics]: https://developer.apple.com/library/archive/documentation/FileManagement/Conceptual/FileSystemProgrammingGuide/FileSystemOverview/FileSystemOverview.html

use std::path::{Path, PathBuf};

use super::PlatformPaths;

/// The bundle id baked into the binary at build time by
/// `emit_app` — falls back to `CARGO_PKG_NAME` when unset.
const BAKED_BUNDLE_ID: Option<&str> = option_env!("ISTMO_APP_BUNDLE_ID");
const CARGO_CRATE_NAME: &str = env!("CARGO_PKG_NAME");

/// Build the default [`PlatformPaths`] for the running desktop OS.
///
/// Callers rarely invoke this directly — [`super::install`] delegates
/// to it on first lookup.
#[must_use]
pub fn default_paths() -> PlatformPaths {
    let id = BAKED_BUNDLE_ID.unwrap_or(CARGO_CRATE_NAME);
    resolve_for_os(id)
}

#[cfg(target_os = "linux")]
fn resolve_for_os(id: &str) -> PlatformPaths {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let xdg = |var: &str, fallback: PathBuf| -> PathBuf {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .map_or(fallback, PathBuf::from)
    };
    let data_home = xdg("XDG_DATA_HOME", home.join(".local/share"));
    let config_home = xdg("XDG_CONFIG_HOME", home.join(".config"));
    let cache_home = xdg("XDG_CACHE_HOME", home.join(".cache"));
    let state_home = xdg("XDG_STATE_HOME", home.join(".local/state"));
    let documents = xdg("XDG_DOCUMENTS_DIR", home.join("Documents"));
    let runtime = std::env::var_os("XDG_RUNTIME_DIR")
        .filter(|v| !v.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    let data = data_home.join(id);
    let config = config_home.join(id);
    let cache = cache_home.join(id);
    let state = state_home.join(id);
    PlatformPaths {
        data_local_dir: data.clone(),
        data_dir: data,
        config_local_dir: config.clone(),
        preference_dir: config.clone(),
        config_dir: config,
        cache_dir: cache,
        state_dir: state,
        documents_dir: documents,
        runtime_dir: runtime,
        temp_dir: std::env::temp_dir(),
    }
}

#[cfg(target_os = "macos")]
fn resolve_for_os(id: &str) -> PlatformPaths {
    let home = home_dir().unwrap_or_else(|| PathBuf::from("/"));
    let lib = home.join("Library");
    let app_support = lib.join("Application Support").join(id);
    let caches = lib.join("Caches").join(id);
    let prefs = lib.join("Preferences").join(id);
    PlatformPaths {
        data_dir: app_support.clone(),
        data_local_dir: app_support.clone(),
        config_dir: app_support.clone(),
        config_local_dir: app_support.clone(),
        cache_dir: caches,
        state_dir: app_support.clone(),
        preference_dir: prefs,
        documents_dir: home.join("Documents"),
        runtime_dir: std::env::temp_dir(),
        temp_dir: std::env::temp_dir(),
    }
}

#[cfg(target_os = "windows")]
fn resolve_for_os(id: &str) -> PlatformPaths {
    let appdata = env_path("APPDATA")
        .unwrap_or_else(|| home_dir().unwrap_or_default().join("AppData/Roaming"));
    let localappdata = env_path("LOCALAPPDATA")
        .unwrap_or_else(|| home_dir().unwrap_or_default().join("AppData/Local"));
    let userprofile = env_path("USERPROFILE").unwrap_or_else(|| home_dir().unwrap_or_default());
    let roaming = appdata.join(id);
    let local = localappdata.join(id);
    PlatformPaths {
        data_dir: roaming.join("data"),
        data_local_dir: local.join("data"),
        config_dir: roaming.join("config"),
        config_local_dir: local.join("config"),
        cache_dir: local.join("cache"),
        state_dir: local.join("state"),
        preference_dir: roaming.join("config"),
        documents_dir: userprofile.join("Documents"),
        runtime_dir: std::env::temp_dir(),
        temp_dir: std::env::temp_dir(),
    }
}

/// Fallback for platforms that never see this branch (Android / iOS
/// install their own [`PlatformPaths`]). Kept so `cargo check` on
/// unusual targets still compiles.
#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn resolve_for_os(_id: &str) -> PlatformPaths {
    let tmp = std::env::temp_dir();
    PlatformPaths {
        data_dir: tmp.clone(),
        data_local_dir: tmp.clone(),
        config_dir: tmp.clone(),
        config_local_dir: tmp.clone(),
        cache_dir: tmp.clone(),
        state_dir: tmp.clone(),
        preference_dir: tmp.clone(),
        documents_dir: tmp.clone(),
        runtime_dir: tmp.clone(),
        temp_dir: tmp,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn home_dir() -> Option<PathBuf> {
    #[cfg(target_os = "windows")]
    {
        env_path("USERPROFILE")
            .or_else(|| env_path("HOMEDRIVE").zip(env_path("HOMEPATH")).map(join_pair))
    }
    #[cfg(not(target_os = "windows"))]
    {
        env_path("HOME")
    }
}

#[cfg(target_os = "windows")]
fn join_pair((a, b): (PathBuf, PathBuf)) -> PathBuf {
    a.join(b)
}

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

// Re-exported for tests below.
#[allow(dead_code)]
fn _touch(_p: &Path) {}

#[cfg(all(test, target_os = "linux"))]
mod linux_tests {
    use super::*;

    #[test]
    fn xdg_overrides_take_precedence() {
        // Best-effort — respects the caller's real env, only reads.
        let paths = resolve_for_os("istmo.test.app");
        // Every path ends with the bundle id (except tmp/documents/runtime).
        assert!(paths.data_dir.ends_with("istmo.test.app"));
        assert!(paths.cache_dir.ends_with("istmo.test.app"));
        assert!(paths.config_dir.ends_with("istmo.test.app"));
    }
}
