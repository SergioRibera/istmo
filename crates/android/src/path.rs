//! Android path provider — takes filesystem paths from Kotlin's
//! `Context` accessors and hands them to
//! [`istmo_core::path::install`].
//!
//! Kotlin invokes [`Java_dev_istmo_runtime_IstmoRuntime_nativeInstallPaths`]
//! with the standard `Context` getters:
//!
//! - `getFilesDir` — persistent internal storage. Backs `data_dir` and
//!   `data_local_dir`; a `config/` subdirectory is created for
//!   `config_dir` and `preference_dir`.
//! - `getCacheDir` — OS-cleanable cache. Backs `cache_dir` and
//!   `runtime_dir`.
//! - `getNoBackupFilesDir` — persistent, excluded from cloud backup.
//!   Backs `state_dir`.
//! - `getExternalFilesDir(null)` — optional shared storage. When
//!   present, backs `documents_dir`; when absent (rare, only on
//!   devices without external storage), `documents_dir` falls back to
//!   `filesDir/documents`.

use istmo_core::path::{PlatformPaths, install};
use std::path::PathBuf;

/// Wire the runtime path provider from four Kotlin-side directories.
///
/// This helper is the plain-Rust counterpart to the JNI method — tests
/// use it to inject fake directories.
pub fn install_from_dirs(
    files: PathBuf,
    cache: PathBuf,
    no_backup: PathBuf,
    external_files: Option<PathBuf>,
) {
    let config = files.join("config");
    let preferences = files.join("preferences");
    let documents = external_files.unwrap_or_else(|| files.join("documents"));
    let _ = std::fs::create_dir_all(&config);
    let _ = std::fs::create_dir_all(&preferences);
    let _ = std::fs::create_dir_all(&documents);
    let paths = PlatformPaths {
        data_dir: files.clone(),
        data_local_dir: files.clone(),
        config_dir: config.clone(),
        config_local_dir: config,
        cache_dir: cache.clone(),
        state_dir: no_backup,
        preference_dir: preferences,
        documents_dir: documents,
        runtime_dir: cache,
        temp_dir: std::env::temp_dir(),
    };
    install(paths);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn install_from_dirs_populates_reasonable_defaults() {
        let tmp = std::env::temp_dir().join("istmo-android-path-test");
        let files = tmp.join("files");
        let cache = tmp.join("cache");
        let nb = tmp.join("nobackup");
        // Call is idempotent through OnceLock — this test verifies it
        // does not panic and creates subdirectories.
        install_from_dirs(files.clone(), cache, nb, None);
        assert!(files.join("config").exists());
        assert!(files.join("preferences").exists());
        assert!(files.join("documents").exists());
    }
}
