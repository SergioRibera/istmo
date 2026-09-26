//! Build-time "doctor": setup mistakes in an app crate reported as
//! `cargo::warning`s with the fix spelled out, on every `cargo build`.
//!
//! It covers what the build script can see — the crate, its
//! `istmo.toml` and the native project files. Toolchain checks (cargo on
//! `PATH`, installed Rust targets, the Android NDK) live where the build
//! is driven from: the `dev.istmo.app` Gradle plugin (`istmoDoctor`
//! task) and the generated Xcode `build-rust.sh`.

use std::fs;
use std::path::{Path, PathBuf};

use crate::app_config::AppMetadata;

/// One problem found by [`Doctor`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub message: String,
}

/// What the doctor inspects: the app metadata plus the native project
/// roots that are active for this build.
#[derive(Debug)]
pub struct Doctor<'a> {
    app: &'a AppMetadata,
    android_root: Option<&'a Path>,
    ios_app_path: Option<PathBuf>,
}

impl<'a> Doctor<'a> {
    #[must_use]
    pub fn new(
        app: &'a AppMetadata,
        android_root: Option<&'a Path>,
        ios_root_and_app_dir: Option<(&'a Path, &'a str)>,
    ) -> Self {
        Self {
            app,
            android_root,
            ios_app_path: ios_root_and_app_dir.map(|(root, dir)| root.join(dir)),
        }
    }

    /// Run every check.
    #[must_use]
    pub fn examine(&self) -> Vec<Finding> {
        let mut findings = Vec::new();
        if let Some(android) = self.android_root {
            self.check_android(android, &mut findings);
        }
        if let Some(ios_app) = &self.ios_app_path {
            self.check_ios(ios_app, &mut findings);
        }
        findings
    }

    /// Print every finding as a `cargo::warning`.
    pub fn report(&self) {
        for finding in self.examine() {
            println!("cargo::warning=istmo doctor: {}", finding.message);
        }
    }

    fn check_android(&self, android: &Path, findings: &mut Vec<Finding>) {
        let krate = &self.app.krate;
        if !krate.has_crate_type("cdylib") {
            findings.push(Finding {
                message: format!(
                    "`android/` exists but `{}` does not build a `cdylib`; add \"cdylib\" to \
                     `[lib] crate-type` in Cargo.toml so Gradle can package lib{}.so",
                    krate.package, krate.lib_name
                ),
            });
        }
        let gradle = ["build.gradle.kts", "build.gradle"]
            .iter()
            .find_map(|name| fs::read_to_string(android.join("app").join(name)).ok());
        let Some(gradle) = gradle else {
            return;
        };
        if !gradle.contains("dev.istmo.app") {
            let message = if gradle.contains("dev.istmo.plugin-loader") {
                "android/app applies `dev.istmo.plugin-loader`, which only links workspace \
                 plugins; switch to `id(\"dev.istmo.app\")` to also build the Rust library, link \
                 plugins from any source and apply `[app]`"
            } else {
                "android/app does not apply the istmo Gradle plugin; add \
                 `id(\"dev.istmo.app\")` to its `plugins {}` block so the Rust library, plugin \
                 sources, manifests and dependencies are wired in"
            };
            findings.push(Finding {
                message: message.to_owned(),
            });
            // The `${istmo*}` manifest placeholders only exist with `dev.istmo.app`.
            return;
        }
        let Ok(manifest) = fs::read_to_string(android.join("app/src/main/AndroidManifest.xml"))
        else {
            return;
        };
        if self.app.icon.is_some() && !manifest.contains("${istmoIcon}") {
            findings.push(Finding {
                message: "`[app] icon` is set but AndroidManifest.xml does not use it; set \
                          `android:icon=\"${istmoIcon}\"` on <application>"
                    .to_owned(),
            });
        }
        if !manifest.contains("${istmoLabel}") {
            findings.push(Finding {
                message: "AndroidManifest.xml hardcodes the app label; set \
                          `android:label=\"${istmoLabel}\"` on <application> to use `[app] name`"
                    .to_owned(),
            });
        }
    }

    fn check_ios(&self, ios_app: &Path, findings: &mut Vec<Finding>) {
        let krate = &self.app.krate;
        if !krate.has_crate_type("staticlib") {
            findings.push(Finding {
                message: format!(
                    "`ios/` exists but `{}` does not build a `staticlib`; add \"staticlib\" to \
                     `[lib] crate-type` in Cargo.toml so Xcode can link lib{}.a",
                    krate.package, krate.lib_name
                ),
            });
        }
        let ios_root = ios_app.parent().unwrap_or(ios_app);
        if let Ok(project) = fs::read_to_string(ios_root.join("project.yml")) {
            if !project.contains("istmo-plugins.yml") {
                findings.push(Finding {
                    message: format!(
                        "ios/project.yml does not include the generated fragment; add \
                         `include: [{{ path: {}/istmo-plugins.yml }}]` so plugin sources, build \
                         settings and the cargo build phase reach the Xcode project",
                        ios_app
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("<App>")
                    ),
                });
            }
        }
        let Ok(plist) = fs::read_to_string(ios_app.join("Info.plist")) else {
            return;
        };
        for (key, variable) in [
            ("CFBundleShortVersionString", "$(MARKETING_VERSION)"),
            ("CFBundleVersion", "$(CURRENT_PROJECT_VERSION)"),
            ("CFBundleDisplayName", "$(ISTMO_DISPLAY_NAME)"),
        ] {
            if plist_value(&plist, key).is_some_and(|value| value != variable) {
                findings.push(Finding {
                    message: format!(
                        "Info.plist hardcodes {key}; set it to `{variable}` so it follows \
                         istmo.toml `[app]`"
                    ),
                });
            }
        }
    }
}

/// Value of a top-level `<key>…</key><string>…</string>` pair — enough
/// for the handful of scalar keys the doctor inspects.
fn plist_value<'s>(plist: &'s str, key: &str) -> Option<&'s str> {
    let after_key = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after_key.find("<string>")? + "<string>".len();
    let end = after_key[start..].find("</string>")? + start;
    Some(after_key[start..end].trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_config::{AppIdentity, CrateInfo};
    use crate::min_versions::MinVersions;

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("istmo-doctor-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn app(crate_types: &[&str], icon: bool) -> AppMetadata {
        AppMetadata::resolve(
            &AppIdentity {
                icon: icon.then(|| PathBuf::from("icon.png")),
                ..AppIdentity::default()
            },
            &MinVersions::default(),
            CrateInfo {
                package: "demo".into(),
                version: "0.1.0".into(),
                lib_name: "demo".into(),
                crate_types: crate_types.iter().map(|&t| t.to_owned()).collect(),
                manifest_dir: PathBuf::from("/work/demo"),
            },
        )
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn flags_missing_gradle_plugin_and_crate_type() {
        let root = temp_dir("android-plugin");
        write(
            &root.join("app/build.gradle.kts"),
            "plugins { id(\"com.android.application\") }",
        );
        let app = app(&["staticlib"], true);
        let findings = Doctor::new(&app, Some(&root), None).examine();
        let text: Vec<&str> = findings.iter().map(|f| f.message.as_str()).collect();
        assert_eq!(findings.len(), 2, "{text:#?}");
        assert!(text[0].contains("cdylib"));
        assert!(text[1].contains("id(\"dev.istmo.app\")"));
    }

    #[test]
    fn suggests_migrating_off_plugin_loader() {
        let root = temp_dir("android-loader");
        write(
            &root.join("app/build.gradle.kts"),
            "plugins { id(\"dev.istmo.plugin-loader\") }",
        );
        let app = app(&["cdylib"], false);
        let findings = Doctor::new(&app, Some(&root), None).examine();
        assert_eq!(findings.len(), 1);
        assert!(findings[0].message.contains("switch to"));
    }

    #[test]
    fn flags_hardcoded_manifest_label_and_icon() {
        let root = temp_dir("android-manifest");
        write(
            &root.join("app/build.gradle.kts"),
            "plugins { id(\"dev.istmo.app\") }",
        );
        write(
            &root.join("app/src/main/AndroidManifest.xml"),
            "<manifest><application android:label=\"Demo\"/></manifest>",
        );
        let app = app(&["cdylib"], true);
        let findings = Doctor::new(&app, Some(&root), None).examine();
        let text: Vec<&str> = findings.iter().map(|f| f.message.as_str()).collect();
        assert_eq!(findings.len(), 2, "{text:#?}");
        assert!(text[0].contains("${istmoIcon}"));
        assert!(text[1].contains("${istmoLabel}"));
    }

    #[test]
    fn clean_android_project_has_no_findings() {
        let root = temp_dir("android-clean");
        write(
            &root.join("app/build.gradle.kts"),
            "plugins { id(\"dev.istmo.app\") }",
        );
        write(
            &root.join("app/src/main/AndroidManifest.xml"),
            "<application android:label=\"${istmoLabel}\" android:icon=\"${istmoIcon}\"/>",
        );
        let app = app(&["cdylib"], true);
        assert!(Doctor::new(&app, Some(&root), None).examine().is_empty());
    }

    #[test]
    fn flags_ios_setup_mistakes() {
        let root = temp_dir("ios");
        write(&root.join("project.yml"), "name: Demo\n");
        write(
            &root.join("Demo/Info.plist"),
            "<dict><key>CFBundleShortVersionString</key><string>0.1.0</string>\
             <key>CFBundleVersion</key><string>$(CURRENT_PROJECT_VERSION)</string></dict>",
        );
        let app = app(&["cdylib"], false);
        let findings = Doctor::new(&app, None, Some((&root, "Demo"))).examine();
        let text: Vec<&str> = findings.iter().map(|f| f.message.as_str()).collect();
        assert_eq!(findings.len(), 3, "{text:#?}");
        assert!(text[0].contains("staticlib"));
        assert!(text[1].contains("Demo/istmo-plugins.yml"));
        assert!(text[2].contains("CFBundleShortVersionString"));
    }

    #[test]
    fn reads_plist_scalars() {
        let plist = "<key>CFBundleVersion</key>\n    <string> 7 </string>";
        assert_eq!(plist_value(plist, "CFBundleVersion"), Some("7"));
        assert_eq!(plist_value(plist, "CFBundleDisplayName"), None);
    }
}
