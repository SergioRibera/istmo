//! iOS path provider — populates [`istmo_core::path::PlatformPaths`]
//! from `NSFileManager` URL lookups.
//!
//! Resolution follows Apple's directory conventions:
//!
//! - `NSApplicationSupportDirectory` → data / config / state.
//! - `NSCachesDirectory` → cache.
//! - `NSDocumentDirectory` → documents.
//! - `NSLibraryDirectory/Preferences` → preferences.
//! - `NSTemporaryDirectory()` → runtime / temp.
//!
//! Domain is always [`NSUserDomainMask`](https://developer.apple.com/documentation/foundation/nsuserdomainmask)
//! (`NSUserDomainMask`). All URLs are eagerly created if missing so the
//! caller can immediately write into any of them.

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
use std::path::PathBuf;

use istmo_core::path::{PlatformPaths, install};

/// Populate `istmo_core::path` with iOS-native directories.
///
/// Idempotent — the second call is a no-op.
pub fn install_from_bundle() {
    install(resolve());
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
fn resolve() -> PlatformPaths {
    use objc2_foundation::{
        NSFileManager, NSSearchPathDirectory, NSSearchPathDomainMask, NSString,
    };

    fn dir_for(kind: NSSearchPathDirectory) -> PathBuf {
        let fm = unsafe { NSFileManager::defaultManager() };
        let urls = unsafe { fm.URLsForDirectory_inDomains(kind, NSSearchPathDomainMask::User) };
        if let Some(url) = urls.first() {
            let path = unsafe { url.path() };
            if let Some(path) = path {
                let _ = std::fs::create_dir_all(PathBuf::from(path.to_string()));
                return PathBuf::from(path.to_string());
            }
        }
        std::env::temp_dir()
    }

    fn temporary_directory() -> PathBuf {
        let ns = unsafe { NSFileManager::defaultManager().temporaryDirectory() };
        let path = unsafe { ns.path() };
        match path {
            Some(p) => PathBuf::from(p.to_string()),
            None => std::env::temp_dir(),
        }
    }

    let app_support = dir_for(NSSearchPathDirectory::NSApplicationSupportDirectory);
    let caches = dir_for(NSSearchPathDirectory::NSCachesDirectory);
    let documents = dir_for(NSSearchPathDirectory::NSDocumentDirectory);
    let library = dir_for(NSSearchPathDirectory::NSLibraryDirectory);
    let preferences = library.join("Preferences");
    let _ = std::fs::create_dir_all(&preferences);
    let temp = temporary_directory();
    PlatformPaths {
        data_dir: app_support.clone(),
        data_local_dir: app_support.clone(),
        config_dir: app_support.clone(),
        config_local_dir: app_support.clone(),
        cache_dir: caches,
        state_dir: app_support,
        preference_dir: preferences,
        documents_dir: documents,
        runtime_dir: temp.clone(),
        temp_dir: temp,
    }
}

#[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
fn resolve() -> PlatformPaths {
    // Fallback for `cargo check` on non-Apple targets. Real Apple
    // builds never reach this branch.
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
