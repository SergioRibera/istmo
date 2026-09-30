//! Filesystem-backed asset backend for desktop targets.
//!
//! # Resolution order
//!
//! On first access the desktop backend picks a root directory using
//! the following ranked lookup (first hit wins):
//!
//! 1. `ISTMO_ASSETS_DIR` — runtime environment variable, always wins.
//!    Installers point this at `/opt/<app>/assets/`, `/usr/share/<app>/`,
//!    …
//! 2. `env!("ISTMO_APP_ASSETS_DIR")` — build-time value baked into the
//!    binary by `emit_app` when `[app] assets_dir = "…"` is set in the
//!    consuming app's `istmo.toml`.
//! 3. `<exe_dir>/assets/` — release-install convention, matches what
//!    `emit_app` copies into `target/<mode>/`.
//! 4. `env!("CARGO_MANIFEST_DIR")/assets/` — the crate root, only
//!    populated in `cargo run` dev builds.
//!
//! # Layout
//!
//! Inside the root directory:
//!
//! - App-owned assets live directly at the root (`<root>/img/logo.png`).
//! - Plugin-owned assets live in a `plugin:<id>/` subdirectory
//!   (`<root>/plugin:istmo.data_store/schema.sql`). This mirrors the
//!   copy layout `emit_app` writes.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use super::{AssetBackend, AssetReader, AssetScope};

const RUNTIME_ENV: &str = "ISTMO_ASSETS_DIR";

/// The build-time env var — `option_env!` so unset means "not baked".
/// Captured in `istmo-core`'s compile environment; the consuming app
/// overrides it via [`set_app_assets`] so `emit_app`'s
/// `cargo::rustc-env=ISTMO_APP_ASSETS_DIR=…` (which only applies to the
/// app crate) is actually observed.
const BUILD_TIME_DIR: Option<&str> = option_env!("ISTMO_APP_ASSETS_DIR");

/// The crate root baked in at compile time — used as the last-resort
/// fallback for `cargo run` dev flows. Same caveat as
/// [`BUILD_TIME_DIR`]: this resolves to `istmo-core`'s manifest dir
/// unless the app crate calls [`set_app_assets`] (which
/// `istmo::runtime!` does automatically).
const CRATE_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

struct AppAssetsOverride {
    build_time_dir: Option<&'static str>,
    manifest_dir: &'static str,
}

static APP_ASSETS_OVERRIDE: OnceLock<AppAssetsOverride> = OnceLock::new();

/// Override the build-time asset-directory strings used by
/// [`resolve_root`].
///
/// Called by `istmo::runtime!`-generated code so `option_env!` /
/// `env!` are evaluated in the app crate's compile environment
/// instead of `istmo-core`'s. Idempotent — the first call wins.
pub fn set_app_assets(build_time_dir: Option<&'static str>, manifest_dir: &'static str) {
    let _ = APP_ASSETS_OVERRIDE.set(AppAssetsOverride {
        build_time_dir,
        manifest_dir,
    });
}

/// Filesystem-rooted [`AssetBackend`] implementation.
///
/// Instances are usually built via [`DesktopAssetBackend::new`], which
/// resolves the root lazily on first use. Tests inject a specific root
/// with [`DesktopAssetBackend::for_root`].
#[derive(Debug)]
pub struct DesktopAssetBackend {
    root: OnceLock<PathBuf>,
    override_root: Option<PathBuf>,
}

impl DesktopAssetBackend {
    #[must_use]
    pub fn new() -> Self {
        Self {
            root: OnceLock::new(),
            override_root: None,
        }
    }

    #[must_use]
    pub fn for_root(root: PathBuf) -> Self {
        Self {
            root: OnceLock::new(),
            override_root: Some(root),
        }
    }

    fn root(&self) -> PathBuf {
        if let Some(root) = &self.override_root {
            return root.clone();
        }
        self.root.get_or_init(resolve_root).clone()
    }

    fn resolve(&self, scope: AssetScope<'_>, key: &str) -> PathBuf {
        let mut path = self.root();
        match scope {
            AssetScope::App => {}
            AssetScope::Plugin(id) => path.push(format!("plugin:{id}")),
        }
        for segment in key.split('/') {
            path.push(segment);
        }
        path
    }
}

impl Default for DesktopAssetBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl AssetBackend for DesktopAssetBackend {
    fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader> {
        let path = self.resolve(scope, key);
        let file = File::open(&path).map_err(|err| annotate(err, &path))?;
        Ok(AssetReader::new(file))
    }
}

fn annotate(err: io::Error, path: &Path) -> io::Error {
    if err.kind() == io::ErrorKind::NotFound {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("asset not found on disk: {}", path.display()),
        )
    } else {
        io::Error::new(err.kind(), format!("{}: {err}", path.display()))
    }
}

fn resolve_root() -> PathBuf {
    let (build_time, manifest) = APP_ASSETS_OVERRIDE
        .get()
        .map(|o| (o.build_time_dir, o.manifest_dir))
        .unwrap_or((BUILD_TIME_DIR, CRATE_MANIFEST_DIR));

    if let Some(raw) = std::env::var_os(RUNTIME_ENV)
        && !raw.is_empty()
    {
        return PathBuf::from(raw);
    }
    if let Some(baked) = build_time
        && !baked.is_empty()
    {
        return PathBuf::from(baked);
    }
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let candidate = parent.join("assets");
        if candidate.is_dir() {
            return candidate;
        }
    }
    PathBuf::from(manifest).join("assets")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    #[test]
    fn env_override_wins() {
        let tmp = std::env::temp_dir().join("istmo-desktop-env-wins");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("x.txt"), b"env").unwrap();
        let backend = DesktopAssetBackend::for_root(tmp);
        let mut r = backend.open(AssetScope::App, "x.txt").unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"env");
    }
}
