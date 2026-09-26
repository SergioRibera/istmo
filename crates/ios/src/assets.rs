//! iOS asset backend.
//!
//! Reads bundled assets from `NSBundle.main.bundlePath` via the standard
//! filesystem — iOS `.app` bundles expose their Resources directly at
//! the bundle root, so `<bundle_path>/<key>` is the correct file path.
//!
//! Plugin-scoped assets live under `plugin:<id>/` subdirectories to
//! match the layout `emit_app` writes into the Xcode project.

use std::fs::File;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use istmo_core::assets::{AssetBackend, AssetReader, AssetScope, install_backend};

/// Filesystem-rooted asset backend anchored at the iOS `.app` bundle
/// directory.
///
/// Instances are usually built via [`IosAssetBackend::main_bundle`],
/// which caches `NSBundle.main.bundlePath` on first use. Tests inject a
/// specific directory with [`IosAssetBackend::for_root`].
#[derive(Debug)]
pub struct IosAssetBackend {
    root: OnceLock<PathBuf>,
    override_root: Option<PathBuf>,
}

impl IosAssetBackend {
    #[must_use]
    pub fn main_bundle() -> Self {
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
        self.root.get_or_init(main_bundle_path).clone()
    }
}

impl AssetBackend for IosAssetBackend {
    fn open(&self, scope: AssetScope<'_>, key: &str) -> io::Result<AssetReader> {
        let mut path = self.root();
        if let AssetScope::Plugin(id) = scope {
            path.push(format!("plugin:{id}"));
        }
        for segment in key.split('/') {
            path.push(segment);
        }
        let file = File::open(&path).map_err(|err| annotate(err, &path))?;
        Ok(AssetReader::new(file))
    }
}

/// Install the iOS asset backend into `istmo-core`.
///
/// Idempotent — a second call is silently ignored.
pub fn install() {
    install_backend(Arc::new(IosAssetBackend::main_bundle()));
}

fn annotate(err: io::Error, path: &Path) -> io::Error {
    if err.kind() == io::ErrorKind::NotFound {
        io::Error::new(
            io::ErrorKind::NotFound,
            format!("iOS asset not found: {}", path.display()),
        )
    } else {
        io::Error::new(err.kind(), format!("{}: {err}", path.display()))
    }
}

#[cfg(any(target_os = "ios", target_os = "tvos", target_os = "visionos"))]
fn main_bundle_path() -> PathBuf {
    use objc2_foundation::NSBundle;
    let bundle = NSBundle::mainBundle();
    let ns_path = bundle.bundlePath();
    PathBuf::from(ns_path.to_string())
}

#[cfg(not(any(target_os = "ios", target_os = "tvos", target_os = "visionos")))]
fn main_bundle_path() -> PathBuf {
    // Fallback for the desktop test / lint / doc build. Real apps
    // running on Apple targets never reach this branch.
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Read;

    #[test]
    fn for_root_reads_scoped_paths() {
        let tmp = std::env::temp_dir().join("istmo-ios-assets-scoped");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(tmp.join("plugin:istmo.demo")).unwrap();
        fs::write(tmp.join("root.txt"), b"root").unwrap();
        fs::write(tmp.join("plugin:istmo.demo/leaf.txt"), b"leaf").unwrap();
        let backend = IosAssetBackend::for_root(tmp);
        let mut r = backend.open(AssetScope::App, "root.txt").unwrap();
        let mut buf = Vec::new();
        r.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, b"root");
        let mut r2 = backend
            .open(AssetScope::Plugin("istmo.demo"), "leaf.txt")
            .unwrap();
        let mut buf2 = Vec::new();
        r2.read_to_end(&mut buf2).unwrap();
        assert_eq!(buf2, b"leaf");
    }
}
