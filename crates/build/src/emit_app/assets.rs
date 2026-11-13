//! Asset staging: every app and plugin `[[assets]]` entry linked (or
//! copied) into the Android, iOS and desktop asset roots.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::assets::{AssetsPayload, ResolvedAsset, resolve_assets};
use crate::handover::collect_dep_assets;
use crate::ios_project::IosProject;
use crate::manifest::Manifest;

/// Stage every dependency + app-declared asset into the platform trees
/// so the runtime backends can serve them at read time.
///
/// - Android: files are placed under
///   `<android_root>/app/src/main/assets/plugin-<id>/…` for plugin
///   assets, and directly under `<android_root>/app/src/main/assets/`
///   for the app's own entries. Gradle's asset merger picks them up on
///   the next build.
/// - iOS: one tree, `<ios_root>/.istmo/Assets/`, app entries at its
///   root and plugin entries under `plugin-<id>/`, shipped as a single
///   folder reference by the generated xcodegen fragment. Files staged
///   by older releases under `<app_dir>/Assets/` and
///   `<app_dir>/<plugins_subdir>/Assets/` are removed.
/// - Desktop: see [`stage_desktop_assets`].
///
/// Each tree remembers what it installed (in `OUT_DIR`) so assets that
/// are no longer declared get removed on the next run.
pub(super) fn sync_assets(
    root: &Path,
    app_manifest: Option<&Manifest>,
    android_dir: Option<&Path>,
    ios: Option<(&Path, &str, &str)>,
) -> Option<PathBuf> {
    let dep_assets = collect_dep_assets();
    let app_assets = app_manifest.map_or_else(Vec::new, |manifest| {
        resolve_assets(&manifest.assets, root).unwrap_or_else(|err| {
            panic!("istmo-build: resolving [app] assets: {err}");
        })
    });
    let state_dir = std::env::var_os("OUT_DIR").map(PathBuf::from);
    let state_dir = state_dir.as_deref();

    if let Some(android_root) = android_dir {
        let mut tree = AssetTree::new(
            android_root.join("app/src/main/assets"),
            state_dir,
            "android",
        );
        tree.install_app(&app_assets);
        tree.install_plugins(&dep_assets);
        tree.finish();
    }

    if let Some((ios_root, app_dir, plugins_subdir)) = ios {
        let project = IosProject::new(ios_root, app_dir);
        let mut tree = AssetTree::new(project.asset_folder_path(), state_dir, "ios");
        tree.install_app(&app_assets);
        tree.install_plugins(&dep_assets);
        let app_path = project.app_path();
        tree.retire_legacy_root(&app_path.join("Assets"), "ios-app");
        tree.retire_legacy_root(&app_path.join(plugins_subdir).join("Assets"), "ios-plugins");
        tree.finish();
    }

    for (_links, payload) in &dep_assets {
        for asset in &payload.resolved {
            crate::directive::rerun_if_changed(asset.source());
        }
    }
    for asset in &app_assets {
        crate::directive::rerun_if_changed(asset.source());
    }
    if let Some(manifest) = app_manifest {
        for dir in crate::assets::declared_asset_dirs(&manifest.assets, root) {
            crate::directive::rerun_if_changed(&dir);
        }
    }

    stage_desktop_assets(&app_assets, &dep_assets, state_dir)
}

/// Materialise every dependency + app asset under `OUT_DIR/istmo_assets/`
/// so `cargo run` on desktop can serve plugin-owned assets without the
/// user hand-copying them into a sibling `assets/` directory.
///
/// The staged root is returned to [`emit_runtime_env`], which bakes it
/// into `ISTMO_APP_ASSETS_DIR` (unless `[app] assets_dir` overrides).
/// Returns `None` when there is nothing to stage — keeps clean-source
/// projects free of `OUT_DIR` clutter.
fn stage_desktop_assets(
    app_assets: &[ResolvedAsset],
    dep_assets: &[(String, AssetsPayload)],
    out_dir: Option<&Path>,
) -> Option<PathBuf> {
    if app_assets.is_empty() && dep_assets.iter().all(|(_, p)| p.resolved.is_empty()) {
        return None;
    }
    let staging = out_dir?.join("istmo_assets");
    let mut tree = AssetTree::new(staging.clone(), out_dir, "desktop");
    tree.install_app(app_assets);
    tree.install_plugins(dep_assets);
    tree.finish();
    Some(staging)
}

/// Directory, under an asset root, holding plugin `id`'s assets — the
/// first of `istmo_core::assets::plugin_asset_dirs`. `-` rather than the
/// `:` older releases used, which NTFS rejects in file names.
fn plugin_asset_dir(id: &str) -> String {
    format!("plugin-{id}")
}

/// Pre-`plugin-<id>` staging directory, removed when found.
fn legacy_plugin_asset_dir(id: &str) -> String {
    format!("{LEGACY_PLUGIN_DIR_PREFIX}{id}")
}

const LEGACY_PLUGIN_DIR_PREFIX: &str = "plugin:";

/// One asset root istmo-build stages files into, plus the record of
/// what the previous run installed there.
#[derive(Debug)]
struct AssetTree {
    root: PathBuf,
    /// `None` outside a build script (no `OUT_DIR`): nothing is pruned.
    state_file: Option<PathBuf>,
    /// Paths installed by this run, relative to `root`, with their source.
    installed: BTreeMap<String, PathBuf>,
}

/// State file recording what the tree `name` installed.
fn state_file_name(name: &str) -> String {
    format!("istmo-assets-{name}.list")
}

impl AssetTree {
    fn new(root: PathBuf, state_dir: Option<&Path>, name: &str) -> Self {
        Self {
            root,
            state_file: state_dir.map(|dir| dir.join(state_file_name(name))),
            installed: BTreeMap::new(),
        }
    }

    fn install_app(&mut self, assets: &[ResolvedAsset]) {
        for asset in assets {
            self.install(asset.source(), asset.bundle_path.clone());
        }
    }

    fn install_plugins(&mut self, payloads: &[(String, AssetsPayload)]) {
        for (_links, payload) in payloads {
            for id in &payload.plugin_ids {
                let legacy = self.root.join(legacy_plugin_asset_dir(id));
                if legacy.is_dir() {
                    let _ = fs::remove_dir_all(&legacy);
                }
                for asset in &payload.resolved {
                    let rel = format!("{}/{}", plugin_asset_dir(id), asset.bundle_path);
                    self.install(asset.source(), rel);
                }
            }
        }
    }

    /// Materialise `source` at `rel`, preferring a symlink and falling
    /// back to a copy (carrying the source mtime) when the platform
    /// refuses — Windows and Docker cross-mounts routinely reject
    /// symlinks. An up-to-date target is left alone so its mtime does
    /// not churn.
    fn install(&mut self, source: &Path, rel: String) {
        let target = self.root.join(&rel);
        self.installed.insert(rel, source.to_path_buf());
        if is_current(source, &target) {
            return;
        }
        if let Some(parent) = target.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::remove_file(&target);
        #[cfg(unix)]
        if std::os::unix::fs::symlink(source, &target).is_ok() {
            return;
        }
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(source, &target).is_ok() {
            return;
        }
        if let Err(err) = copy_with_mtime(source, &target) {
            crate::directive::warning(&format!(
                "failed to copy asset `{}` → `{}`: {err}",
                source.display(),
                target.display()
            ));
        }
    }

    /// Delete what the previous run installed and this one did not,
    /// then record this run's set.
    fn finish(self) {
        let Some(state_file) = &self.state_file else {
            return;
        };
        if let Ok(previous) = fs::read_to_string(state_file) {
            for rel in previous
                .lines()
                .filter(|rel| !self.installed.contains_key(*rel))
            {
                remove_staged(&self.root, rel);
            }
        }
        let mut listing = String::new();
        for rel in self.installed.keys() {
            listing.push_str(rel);
            listing.push('\n');
        }
        crate::fsx::write_generated(state_file, &listing);
    }

    /// Empty `root`, an asset root older releases staged into, of what
    /// istmo-build put there: every path its state file (`name`) lists,
    /// every `plugin:<id>` directory, and every path still serving one
    /// of this run's sources — a fresh `OUT_DIR` has no state file.
    /// Anything else is the user's and stays. `root` itself goes once
    /// empty.
    fn retire_legacy_root(&self, root: &Path, name: &str) {
        let legacy_state = self
            .state_file
            .as_deref()
            .and_then(Path::parent)
            .map(|dir| dir.join(state_file_name(name)));
        if root.is_dir() {
            let listed = legacy_state
                .as_deref()
                .and_then(|file| fs::read_to_string(file).ok())
                .unwrap_or_default();
            for rel in listed.lines() {
                let target = root.join(rel);
                let is_source = self
                    .installed
                    .get(rel)
                    .is_some_and(|source| is_same_file(source, &target));
                if !is_source {
                    remove_staged(root, rel);
                }
            }
            for (rel, source) in &self.installed {
                let target = root.join(rel);
                if is_current(source, &target) && !is_same_file(source, &target) {
                    remove_staged(root, rel);
                }
            }
            for entry in fs::read_dir(root).into_iter().flatten().flatten() {
                let legacy_plugin_dir = entry
                    .file_name()
                    .to_str()
                    .is_some_and(|name| name.starts_with(LEGACY_PLUGIN_DIR_PREFIX));
                if legacy_plugin_dir && entry.path().is_dir() {
                    let _ = fs::remove_dir_all(entry.path());
                }
            }
            let _ = fs::remove_dir(root);
        }
        if let Some(file) = legacy_state {
            let _ = fs::remove_file(file);
        }
    }
}

/// Remove the staged file at `root/rel`, then the directories the
/// removal emptied — never `root` itself.
fn remove_staged(root: &Path, rel: &str) {
    let path = root.join(rel);
    if fs::symlink_metadata(&path).is_ok_and(|meta| !meta.is_dir()) {
        let _ = fs::remove_file(&path);
    }
    let mut dir = path.parent();
    while let Some(current) = dir {
        if current == root || !current.starts_with(root) {
            break;
        }
        if fs::remove_dir(current).is_err() {
            break;
        }
        dir = current.parent();
    }
}

/// Whether `target` already serves `source`: a symlink pointing at it,
/// or a copy with the same size and mtime.
fn is_current(source: &Path, target: &Path) -> bool {
    let Ok(meta) = fs::symlink_metadata(target) else {
        return false;
    };
    if meta.file_type().is_symlink() {
        return fs::read_link(target).is_ok_and(|link| link == source);
    }
    let Ok(source_meta) = fs::metadata(source) else {
        return false;
    };
    meta.is_file()
        && meta.len() == source_meta.len()
        && meta
            .modified()
            .ok()
            .is_some_and(|mtime| source_meta.modified().ok() == Some(mtime))
}

/// Whether `target` is `source` itself rather than a link or copy —
/// an app may declare a file that sits inside a legacy asset root.
fn is_same_file(source: &Path, target: &Path) -> bool {
    let is_link = fs::symlink_metadata(target).is_ok_and(|meta| meta.file_type().is_symlink());
    !is_link
        && fs::canonicalize(source)
            .ok()
            .is_some_and(|source| fs::canonicalize(target).ok() == Some(source))
}

fn copy_with_mtime(source: &Path, target: &Path) -> std::io::Result<()> {
    fs::copy(source, target)?;
    let mtime = fs::metadata(source)?.modified()?;
    fs::File::options()
        .write(true)
        .open(target)?
        .set_modified(mtime)
}
#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    fn asset_fixture(name: &str) -> (PathBuf, Vec<ResolvedAsset>) {
        let dir =
            std::env::temp_dir().join(format!("istmo-asset-tree-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let assets = ["a.txt", "sub/b.txt"]
            .into_iter()
            .map(|rel| {
                let source = dir.join("src").join(rel);
                write(&source, rel);
                ResolvedAsset {
                    bundle_path: rel.to_owned(),
                    source_abs: source.display().to_string(),
                }
            })
            .collect();
        (dir, assets)
    }

    fn mtime(path: &Path) -> std::time::SystemTime {
        fs::symlink_metadata(path).unwrap().modified().unwrap()
    }

    #[test]
    fn asset_tree_reinstall_keeps_targets_untouched() {
        let (dir, assets) = asset_fixture("idempotent");
        let stage = |assets: &[ResolvedAsset]| {
            let mut tree = AssetTree::new(dir.join("out"), Some(&dir), "test");
            tree.install_app(assets);
            tree.finish();
        };
        stage(&assets);
        let target = dir.join("out/sub/b.txt");
        assert_eq!(fs::read_to_string(&target).unwrap(), "sub/b.txt");
        let before = mtime(&target);
        std::thread::sleep(std::time::Duration::from_millis(20));
        stage(&assets);
        assert_eq!(mtime(&target), before);
    }

    #[test]
    fn asset_tree_prunes_assets_no_longer_declared() {
        let (dir, assets) = asset_fixture("prune");
        let stage = |assets: &[ResolvedAsset]| {
            let mut tree = AssetTree::new(dir.join("out"), Some(&dir), "test");
            tree.install_app(assets);
            tree.finish();
        };
        stage(&assets);
        stage(&assets[..1]);
        assert!(dir.join("out/a.txt").exists());
        assert!(!dir.join("out/sub").exists(), "emptied dir must go too");
    }

    #[test]
    fn plugin_assets_use_a_windows_safe_dir_and_drop_the_legacy_one() {
        let (dir, assets) = asset_fixture("plugin");
        let legacy = dir.join("out").join(legacy_plugin_asset_dir("istmo.demo"));
        write(&legacy.join("a.txt"), "stale");
        let mut tree = AssetTree::new(dir.join("out"), None, "test");
        tree.install_plugins(&[(
            "DEMO".to_owned(),
            AssetsPayload {
                plugin_ids: vec!["istmo.demo".to_owned()],
                resolved: assets,
            },
        )]);
        tree.finish();
        assert!(dir.join("out/plugin-istmo.demo/a.txt").exists());
        assert!(!legacy.exists());
    }

    fn demo_plugin(assets: Vec<ResolvedAsset>) -> Vec<(String, AssetsPayload)> {
        vec![(
            "DEMO".to_owned(),
            AssetsPayload {
                plugin_ids: vec!["istmo.demo".to_owned()],
                resolved: assets,
            },
        )]
    }

    #[test]
    fn ios_assets_share_one_tree_and_retire_the_legacy_roots() {
        let (dir, assets) = asset_fixture("ios-retire");
        let app_root = dir.join("App/Assets");
        let plugin_root = dir.join("App/Plugins/Assets");
        // What an older release left behind: the app tree recorded in its
        // state file, the plugin tree without one (fresh `OUT_DIR`).
        let mut old_app = AssetTree::new(app_root.clone(), Some(&dir), "ios-app");
        old_app.install_app(&assets);
        old_app.finish();
        let mut old_plugins = AssetTree::new(plugin_root.clone(), None, "ios-plugins");
        old_plugins.install_plugins(&demo_plugin(assets.clone()));
        old_plugins.finish();
        write(&app_root.join("mine.txt"), "user file");
        write(&plugin_root.join("plugin:istmo.gone/x.txt"), "legacy");

        let mut tree = AssetTree::new(dir.join(".istmo/Assets"), Some(&dir), "ios");
        tree.install_app(&assets[..1]);
        tree.install_plugins(&demo_plugin(assets.clone()));
        tree.retire_legacy_root(&app_root, "ios-app");
        tree.retire_legacy_root(&plugin_root, "ios-plugins");
        tree.finish();

        assert!(dir.join(".istmo/Assets/a.txt").exists());
        assert!(
            dir.join(".istmo/Assets/plugin-istmo.demo/sub/b.txt")
                .exists()
        );
        // `sub/b.txt` is no longer an app asset but the state file named it.
        assert!(!app_root.join("sub").exists());
        assert!(!app_root.join("a.txt").exists());
        assert!(app_root.join("mine.txt").exists(), "user files stay");
        assert!(!plugin_root.exists(), "emptied legacy root goes");
        assert!(!dir.join(state_file_name("ios-app")).exists());
    }

    #[test]
    fn retiring_a_legacy_root_spares_declared_sources_inside_it() {
        let dir = std::env::temp_dir().join(format!("istmo-asset-own-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let legacy = dir.join("App/Assets");
        let source = legacy.join("logo.txt");
        write(&source, "logo");
        let asset = ResolvedAsset {
            bundle_path: "logo.txt".to_owned(),
            source_abs: source.display().to_string(),
        };
        write(&dir.join(state_file_name("ios-app")), "logo.txt\n");
        let mut tree = AssetTree::new(dir.join(".istmo/Assets"), Some(&dir), "ios");
        tree.install_app(&[asset]);
        tree.retire_legacy_root(&legacy, "ios-app");
        tree.finish();
        assert_eq!(fs::read_to_string(&source).unwrap(), "logo");
    }
}
