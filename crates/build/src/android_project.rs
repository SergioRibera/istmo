//! `android/.istmo/` — what an app's `build.rs` hands to the
//! `dev.istmo.app` Gradle plugin.
//!
//! [`ANDROID_METADATA_FILE`] lists every linked plugin (its
//! `native/android/` sources, manifest, resources and minimum API
//! level), the aggregated `[[gradle]]` dependencies and the app identity
//! from `[app]`. Paths come from the `DEP_*` handover, so plugins pulled
//! from crates.io or git link exactly like workspace members — the
//! Android twin of the `istmo-plugins.yml` xcodegen fragment, and of
//! Flutter's `.flutter-plugins-dependencies`.
//!
//! The file is only written while compiling for Android: target-scoped
//! dependencies make the plugin set target dependent, and a desktop
//! `cargo build` must not clobber it.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::app_config::AppMetadata;
use crate::app_icon::{ANDROID_ICON_RESOURCE, AppIcon, IconError};
use crate::manifest::Manifest;
use crate::native_deps::NativeDeps;

/// Metadata file, relative to the Android project root.
pub const ANDROID_METADATA_FILE: &str = ".istmo/istmo.json";

/// Generated resource directory (launcher icon), relative to the
/// Android project root.
pub const ANDROID_RES_DIR: &str = ".istmo/res";

/// Bumped whenever the JSON shape changes incompatibly; the Gradle
/// plugin refuses files it does not understand.
const SCHEMA: u32 = 1;

/// One plugin crate's Android side, as linked into the app.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AndroidPlugin {
    /// Primary plugin id, or the crate's `links` name for crates that
    /// ship native sources without declaring a plugin.
    pub id: String,
    pub native_dir: PathBuf,
    /// `native/android/AndroidManifest.xml`, merged into every variant.
    pub manifest: Option<PathBuf>,
    /// `native/android/res/`, added as a resource directory.
    pub res: Option<PathBuf>,
    /// `[min_versions] android`.
    pub min_sdk: Option<u32>,
}

impl AndroidPlugin {
    /// Pair every advertised `native/android/` directory with the
    /// manifest of the same crate (matched by `links` name).
    #[must_use]
    pub fn link(native_dirs: &[(String, PathBuf)], manifests: &[(String, Manifest)]) -> Vec<Self> {
        native_dirs
            .iter()
            .map(|(links, dir)| {
                let manifest = manifests
                    .iter()
                    .find(|(name, _)| name == links)
                    .map(|(_, m)| m);
                Self {
                    id: manifest
                        .and_then(|m| m.plugins.first())
                        .map_or_else(|| links.to_lowercase(), |p| p.id.clone()),
                    native_dir: dir.clone(),
                    manifest: Some(dir.join("AndroidManifest.xml")).filter(|p| p.is_file()),
                    res: Some(dir.join("res")).filter(|p| p.is_dir()),
                    min_sdk: manifest.and_then(|m| m.min_versions.android),
                }
            })
            .collect()
    }
}

#[derive(Debug, Serialize)]
struct Metadata<'a> {
    schema: u32,
    package: &'a str,
    lib_name: &'a str,
    manifest_path: PathBuf,
    app: App<'a>,
    plugins: &'a [AndroidPlugin],
    gradle: Vec<GradleEntry>,
}

#[derive(Debug, Serialize)]
struct App<'a> {
    id: Option<&'a str>,
    name: &'a str,
    version_name: &'a str,
    version_code: u32,
    min_sdk: Option<u32>,
    /// Drawable reference for `${istmoIcon}`, when `[app] icon` is set.
    icon: Option<String>,
}

#[derive(Debug, Serialize)]
struct GradleEntry {
    scope: &'static str,
    notation: String,
}

/// The consuming app's Gradle project (`<crate>/android/`).
#[derive(Debug, Clone)]
pub struct AndroidProject {
    root: PathBuf,
}

impl AndroidProject {
    #[must_use]
    pub fn new(root: &Path) -> Self {
        Self {
            root: root.to_path_buf(),
        }
    }

    #[must_use]
    pub fn metadata_path(&self) -> PathBuf {
        self.root.join(ANDROID_METADATA_FILE)
    }

    /// Render [`ANDROID_METADATA_FILE`].
    #[must_use]
    pub fn render_metadata(
        &self,
        app: &AppMetadata,
        plugins: &[AndroidPlugin],
        deps: &NativeDeps,
    ) -> String {
        let metadata = Metadata {
            schema: SCHEMA,
            package: &app.krate.package,
            lib_name: &app.krate.lib_name,
            manifest_path: app.krate.manifest_dir.join("Cargo.toml"),
            app: App {
                id: app.id.as_ref().map(crate::app_config::AppId::as_str),
                name: &app.name,
                version_name: &app.version,
                version_code: app.build,
                min_sdk: app.min_android,
                icon: app
                    .icon
                    .as_ref()
                    .map(|_| format!("@mipmap/{ANDROID_ICON_RESOURCE}")),
            },
            plugins,
            gradle: deps
                .gradle_entries()
                .map(|dep| GradleEntry {
                    scope: dep.scope.as_gradle_str(),
                    notation: dep.coord.as_notation(),
                })
                .collect(),
        };
        let mut json = serde_json::to_string_pretty(&metadata).unwrap_or_else(|err| {
            panic!("istmo-build: serialising {ANDROID_METADATA_FILE}: {err}")
        });
        json.push('\n');
        json
    }

    /// Write the launcher icon mipmaps under [`ANDROID_RES_DIR`].
    pub fn write_icon(&self, icon: &AppIcon) -> Result<(), IconError> {
        icon.write_android_mipmaps(&self.root.join(ANDROID_RES_DIR))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::{AppId, AppIdentity, CrateInfo};
    use crate::min_versions::MinVersions;
    use crate::native_deps::{GradleCoord, GradleDep, GradleScope};

    fn app(icon: bool) -> AppMetadata {
        let identity = AppIdentity {
            id: Some(AppId::try_from("dev.istmo.demo").unwrap()),
            icon: icon.then(|| PathBuf::from("icon.png")),
            ..AppIdentity::default()
        };
        AppMetadata::resolve(
            &identity,
            &MinVersions {
                android: Some(26),
                ..MinVersions::default()
            },
            CrateInfo {
                package: "demo-app".into(),
                version: "0.2.0".into(),
                lib_name: "demo_app".into(),
                crate_types: vec!["cdylib".into()],
                manifest_dir: PathBuf::from("/work/demo-app"),
            },
        )
    }

    #[test]
    fn renders_app_plugins_and_gradle_deps() {
        let plugins = vec![AndroidPlugin {
            id: "istmo.share".into(),
            native_dir: PathBuf::from("/reg/istmo-share/native/android"),
            manifest: Some(PathBuf::from(
                "/reg/istmo-share/native/android/AndroidManifest.xml",
            )),
            res: None,
            min_sdk: Some(22),
        }];
        let mut deps = NativeDeps::new();
        deps.add_gradle(&GradleDep::new(
            GradleScope::Implementation,
            GradleCoord::new("androidx.core", "core-ktx", "1.13.1"),
        ));
        let json = AndroidProject::new(Path::new("/work/demo-app/android")).render_metadata(
            &app(true),
            &plugins,
            &deps,
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["package"], "demo-app");
        assert_eq!(value["lib_name"], "demo_app");
        assert_eq!(value["manifest_path"], "/work/demo-app/Cargo.toml");
        assert_eq!(value["app"]["id"], "dev.istmo.demo");
        assert_eq!(value["app"]["version_name"], "0.2.0");
        assert_eq!(value["app"]["version_code"], 1);
        assert_eq!(value["app"]["min_sdk"], 26);
        assert_eq!(value["app"]["icon"], "@mipmap/istmo_launcher");
        assert_eq!(value["plugins"][0]["id"], "istmo.share");
        assert_eq!(value["plugins"][0]["min_sdk"], 22);
        assert!(value["plugins"][0]["res"].is_null());
        assert_eq!(value["gradle"][0]["scope"], "implementation");
        assert_eq!(
            value["gradle"][0]["notation"],
            "androidx.core:core-ktx:1.13.1"
        );
    }

    #[test]
    fn icon_is_null_without_app_icon() {
        let json = AndroidProject::new(Path::new("/x/android")).render_metadata(
            &app(false),
            &[],
            &NativeDeps::new(),
        );
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(value["app"]["icon"].is_null());
    }

    #[test]
    fn links_plugins_by_links_name() {
        let manifest =
            Manifest::parse("[plugin]\nid = \"istmo.share\"\n[min_versions]\nandroid = 22\n")
                .unwrap();
        let linked = AndroidPlugin::link(
            &[
                ("ISTMO_SHARE".into(), PathBuf::from("/nope/share")),
                ("OTHER".into(), PathBuf::from("/nope/other")),
            ],
            &[("ISTMO_SHARE".into(), manifest)],
        );
        assert_eq!(linked[0].id, "istmo.share");
        assert_eq!(linked[0].min_sdk, Some(22));
        assert_eq!(linked[1].id, "other");
        assert_eq!(linked[1].min_sdk, None);
        assert!(linked[0].manifest.is_none());
    }
}
