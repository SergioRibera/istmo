//! The `[app]` section of an app crate's `istmo.toml`.
//!
//! It answers two questions:
//!
//! - **Codegen** — where the generated Kotlin / Swift goes and which
//!   plugins get registered (`android_package`, `ios_app_dir`,
//!   `[[app.plugin]]`, …).
//! - **Identity** — the app id, display name, version, build number and
//!   launcher icon. `istmo.toml` is the single source of truth for them:
//!   the Gradle plugin (`dev.istmo.app`) applies them to the Android
//!   project and the generated `ios/.istmo/Istmo.xcconfig` to the Xcode
//!   project, so neither `build.gradle.kts` nor `project.yml` repeat them.
//!
//! Parsing is strict: an unknown key or a value of the wrong shape fails
//! the build instead of being silently ignored.

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::manifest::{
    InfoPlistEntry, ManifestError, expect_bool, expect_integer, expect_string, expect_table,
};
use crate::min_versions::{MinVersions, OsVersion};

const KNOWN_APP_KEYS: &[&str] = &[
    "android",
    "android_package",
    "ios",
    "ios_app_dir",
    "ios_plugins_subdir",
    "auto_register",
    "rust_entry",
    "info_plist",
    "plugin",
    "id",
    "name",
    "version",
    "build",
    "icon",
];

const KNOWN_APP_PLUGIN_KEYS: &[&str] = &["id", "type", "role", "platforms", "auto_register"];

/// Largest `versionCode` Google Play accepts.
const MAX_BUILD_NUMBER: i64 = 2_100_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Platform {
    Android,
    Ios,
}

impl Platform {
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "android" => Some(Self::Android),
            "ios" => Some(Self::Ios),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Host,
    Client,
}

impl Role {
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "host" => Some(Self::Host),
            "client" => Some(Self::Client),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct AppPluginOpts {
    pub role: Option<Role>,
    pub platforms: Option<Vec<Platform>>,
    pub auto_register: Option<bool>,
}

/// Reverse-DNS application identifier, used as the Android
/// `applicationId` and the iOS `PRODUCT_BUNDLE_IDENTIFIER`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AppId(String);

impl AppId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<&str> for AppId {
    type Error = ManifestError;

    /// Accepts at least two dot-separated segments, each starting with
    /// an ASCII letter and made of ASCII letters, digits and `_` — the
    /// intersection of what Android and Apple accept.
    fn try_from(raw: &str) -> Result<Self, Self::Error> {
        let valid_segment = |segment: &str| {
            let mut chars = segment.chars();
            chars.next().is_some_and(|c| c.is_ascii_alphabetic())
                && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        if raw.split('.').count() >= 2 && raw.split('.').all(valid_segment) {
            Ok(Self(raw.to_owned()))
        } else {
            Err(ManifestError::InvalidValue {
                key: "app.id".to_owned(),
                value: raw.to_owned(),
                expected: "a reverse-DNS id such as `com.example.app`",
            })
        }
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// App identity keys of `[app]`, as written. Unset keys fall back to
/// Cargo metadata when resolved into an [`AppMetadata`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppIdentity {
    pub id: Option<AppId>,
    /// Display name (launcher label / `CFBundleDisplayName`).
    pub name: Option<String>,
    /// User-facing version. Defaults to the crate's `package.version`.
    pub version: Option<String>,
    /// Monotonic build number (`versionCode` / `CFBundleVersion`).
    pub build: Option<u32>,
    /// Launcher icon source image, relative to the crate root.
    pub icon: Option<PathBuf>,
}

/// Parsed `[app]` section.
#[derive(Debug, Clone, Default)]
pub struct AppConfig {
    pub android: Option<bool>,
    pub android_package: Option<String>,
    pub ios: Option<bool>,
    pub ios_app_dir: Option<String>,
    pub ios_plugins_subdir: Option<String>,
    pub auto_register: Option<bool>,
    /// Whether the crate exports a `#[istmo::mobile_app]` entry point.
    /// Unset means "detect from the crate sources".
    pub rust_entry: Option<bool>,
    pub per_plugin: HashMap<String, AppPluginOpts>,
    pub info_plist: Vec<InfoPlistEntry>,
    pub identity: AppIdentity,
}

impl AppConfig {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let doc: DocumentMut = source.parse().map_err(ManifestError::Parse)?;
        Self::from_document(&doc)
    }

    /// Read the `[app]` section of the `istmo.toml` at `path`. A missing
    /// file is an empty configuration, not an error.
    pub fn from_path(path: &Path) -> Result<Self, ManifestError> {
        match fs::read_to_string(path) {
            Ok(text) => Self::parse(&text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(ManifestError::Io {
                path: path.to_path_buf(),
                error,
            }),
        }
    }

    fn from_document(doc: &DocumentMut) -> Result<Self, ManifestError> {
        let Some(item) = doc.get("app") else {
            return Ok(Self::default());
        };
        let table = expect_table(item, "app")?;
        for (name, _) in table.iter() {
            if !KNOWN_APP_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: format!("app.{name}"),
                });
            }
        }
        let string = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_string(item, full).map(str::to_owned))
                .transpose()
        };
        let boolean = |key: &'static str, full: &'static str| {
            table
                .get(key)
                .map(|item| expect_bool(item, full))
                .transpose()
        };
        let info_plist = match table.get("info_plist") {
            Some(item) => InfoPlistEntry::parse_table(
                expect_table(item, "app.info_plist")?,
                "app.info_plist",
            )?,
            None => Vec::new(),
        };
        Ok(Self {
            android: boolean("android", "app.android")?,
            android_package: string("android_package", "app.android_package")?,
            ios: boolean("ios", "app.ios")?,
            ios_app_dir: string("ios_app_dir", "app.ios_app_dir")?,
            ios_plugins_subdir: string("ios_plugins_subdir", "app.ios_plugins_subdir")?,
            auto_register: boolean("auto_register", "app.auto_register")?,
            rust_entry: boolean("rust_entry", "app.rust_entry")?,
            per_plugin: parse_per_plugin(table.get("plugin"))?,
            info_plist,
            identity: AppIdentity {
                id: string("id", "app.id")?
                    .map(|id| AppId::try_from(id.as_str()))
                    .transpose()?,
                name: string("name", "app.name")?,
                version: string("version", "app.version")?,
                build: table.get("build").map(parse_build_number).transpose()?,
                icon: string("icon", "app.icon")?.map(PathBuf::from),
            },
        })
    }
}

fn parse_build_number(item: &Item) -> Result<u32, ManifestError> {
    let raw = expect_integer(item, "app.build")?;
    if (1..=MAX_BUILD_NUMBER).contains(&raw) {
        u32::try_from(raw).map_err(|_| unreachable_build_number(raw))
    } else {
        Err(unreachable_build_number(raw))
    }
}

fn unreachable_build_number(raw: i64) -> ManifestError {
    ManifestError::InvalidValue {
        key: "app.build".to_owned(),
        value: raw.to_string(),
        expected: "an integer between 1 and 2100000000",
    }
}

fn parse_per_plugin(item: Option<&Item>) -> Result<HashMap<String, AppPluginOpts>, ManifestError> {
    let Some(item) = item else {
        return Ok(HashMap::new());
    };
    let tables: Vec<&Table> = if let Some(table) = item.as_table() {
        vec![table]
    } else if let Some(array) = item.as_array_of_tables() {
        array.iter().collect()
    } else {
        return Err(ManifestError::TypeMismatch {
            key: "app.plugin".to_owned(),
            expected: "table ([app.plugin]) or array of tables ([[app.plugin]])",
        });
    };
    tables
        .into_iter()
        .map(parse_plugin_opts)
        .collect::<Result<_, _>>()
}

fn parse_plugin_opts(table: &Table) -> Result<(String, AppPluginOpts), ManifestError> {
    for (name, _) in table {
        if !KNOWN_APP_PLUGIN_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("app.plugin.{name}"),
            });
        }
    }
    let key = match (table.get("id"), table.get("type")) {
        (Some(item), _) => expect_string(item, "app.plugin.id")?,
        (None, Some(item)) => expect_string(item, "app.plugin.type")?,
        (None, None) => {
            return Err(ManifestError::Missing {
                key: "app.plugin.id",
            });
        }
    };
    let role = table
        .get("role")
        .map(|item| {
            let literal = expect_string(item, "app.plugin.role")?;
            Role::parse(literal).ok_or_else(|| ManifestError::InvalidValue {
                key: "app.plugin.role".to_owned(),
                value: literal.to_owned(),
                expected: "`host` or `client`",
            })
        })
        .transpose()?;
    let platforms = table.get("platforms").map(parse_platforms).transpose()?;
    let auto_register = table
        .get("auto_register")
        .map(|item| expect_bool(item, "app.plugin.auto_register"))
        .transpose()?;
    Ok((
        key.to_owned(),
        AppPluginOpts {
            role,
            platforms,
            auto_register,
        },
    ))
}

fn parse_platforms(item: &Item) -> Result<Vec<Platform>, ManifestError> {
    let mismatch = || ManifestError::TypeMismatch {
        key: "app.plugin.platforms".to_owned(),
        expected: "array of strings",
    };
    let array = item.as_array().ok_or_else(mismatch)?;
    array
        .iter()
        .map(|value| {
            let Value::String(s) = value else {
                return Err(mismatch());
            };
            let literal = s.value();
            Platform::parse(literal).ok_or_else(|| ManifestError::InvalidValue {
                key: "app.plugin.platforms".to_owned(),
                value: literal.clone(),
                expected: "`android` or `ios`",
            })
        })
        .collect()
}

/// Crate facts the build script knows but `istmo.toml` does not repeat.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrateInfo {
    /// Cargo package name (`-p` argument).
    pub package: String,
    pub version: String,
    /// Artifact stem: `lib<lib_name>.so` / `lib<lib_name>.a`.
    pub lib_name: String,
    /// `[lib] crate-type`, empty when the crate does not set it.
    pub crate_types: Vec<String>,
    pub manifest_dir: PathBuf,
}

impl CrateInfo {
    /// Build from the `CARGO_PKG_*` variables Cargo hands a build script
    /// plus the `[lib]` table of `<manifest_dir>/Cargo.toml`.
    #[must_use]
    pub fn from_build_env(manifest_dir: &Path) -> Self {
        let package = std::env::var("CARGO_PKG_NAME").unwrap_or_else(|_| "istmo-app".to_owned());
        let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".to_owned());
        let lib_table = fs::read_to_string(manifest_dir.join("Cargo.toml"))
            .ok()
            .and_then(|text| text.parse::<DocumentMut>().ok())
            .and_then(|doc| doc.get("lib").and_then(Item::as_table).cloned());
        let lib_name = lib_table
            .as_ref()
            .and_then(|lib| lib.get("name"))
            .and_then(Item::as_str)
            .map_or_else(|| package.replace('-', "_"), str::to_owned);
        let crate_types = lib_table
            .as_ref()
            .and_then(|lib| lib.get("crate-type"))
            .and_then(Item::as_array)
            .map(|array| {
                array
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        Self {
            package,
            version,
            lib_name,
            crate_types,
            manifest_dir: manifest_dir.to_path_buf(),
        }
    }

    #[must_use]
    pub fn has_crate_type(&self, crate_type: &str) -> bool {
        self.crate_types.iter().any(|t| t == crate_type)
    }

    /// Whether any source file under `src/` carries a
    /// `#[istmo::mobile_app]` (or `#[mobile_app]`) attribute — i.e. the
    /// crate exports the `istmo_run_ios` entry point.
    #[must_use]
    pub fn declares_mobile_app(&self) -> bool {
        fn scan(dir: &Path) -> bool {
            let Ok(entries) = fs::read_dir(dir) else {
                return false;
            };
            entries.filter_map(Result::ok).any(|entry| {
                let path = entry.path();
                if path.is_dir() {
                    scan(&path)
                } else {
                    path.extension().is_some_and(|ext| ext == "rs")
                        && fs::read_to_string(&path).is_ok_and(|text| {
                            text.lines().any(|line| {
                                let line = line.trim_start();
                                line.starts_with("#[") && line.contains("mobile_app")
                            })
                        })
                }
            })
        }
        scan(&self.manifest_dir.join("src"))
    }
}

/// App identity with every Cargo fallback applied — what the native
/// projects actually receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppMetadata {
    pub id: Option<AppId>,
    pub name: String,
    pub version: String,
    pub build: u32,
    /// Absolute path of the icon source image.
    pub icon: Option<PathBuf>,
    pub min_android: Option<u32>,
    pub min_ios: Option<OsVersion>,
    pub krate: CrateInfo,
}

impl AppMetadata {
    #[must_use]
    pub fn resolve(identity: &AppIdentity, min_versions: &MinVersions, krate: CrateInfo) -> Self {
        Self {
            id: identity.id.clone(),
            name: identity
                .name
                .clone()
                .unwrap_or_else(|| krate.package.clone()),
            version: identity
                .version
                .clone()
                .unwrap_or_else(|| krate.version.clone()),
            build: identity.build.unwrap_or(1),
            icon: identity
                .icon
                .as_ref()
                .map(|icon| krate.manifest_dir.join(icon)),
            min_android: min_versions.android,
            min_ios: min_versions.ios,
            krate,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_app_section_is_default() {
        let cfg = AppConfig::parse("[min_versions]\nandroid = 24\n").unwrap();
        assert!(cfg.android_package.is_none());
        assert_eq!(cfg.identity, AppIdentity::default());
    }

    #[test]
    fn parses_codegen_keys() {
        let cfg = AppConfig::parse(
            r#"
[app]
android_package = "com.example.app.generated"
ios_app_dir = "MyApp"
android = false
ios = true

[[app.plugin]]
id = "istmo.echo"
role = "client"
platforms = ["android"]
"#,
        )
        .unwrap();
        assert_eq!(
            cfg.android_package.as_deref(),
            Some("com.example.app.generated")
        );
        assert_eq!(cfg.ios_app_dir.as_deref(), Some("MyApp"));
        assert_eq!(cfg.android, Some(false));
        assert_eq!(cfg.ios, Some(true));
        let per = &cfg.per_plugin["istmo.echo"];
        assert_eq!(per.role, Some(Role::Client));
        assert_eq!(per.platforms.as_deref(), Some(&[Platform::Android][..]));
    }

    #[test]
    fn parses_identity() {
        let cfg = AppConfig::parse(
            r#"
[app]
id = "dev.istmo.demo"
name = "istmo demo"
version = "1.2.3"
build = 42
icon = "assets/icon.png"
"#,
        )
        .unwrap();
        assert_eq!(cfg.identity.id.unwrap().as_str(), "dev.istmo.demo");
        assert_eq!(cfg.identity.name.as_deref(), Some("istmo demo"));
        assert_eq!(cfg.identity.version.as_deref(), Some("1.2.3"));
        assert_eq!(cfg.identity.build, Some(42));
        assert_eq!(cfg.identity.icon, Some(PathBuf::from("assets/icon.png")));
    }

    #[test]
    fn rejects_unknown_app_key() {
        let err = AppConfig::parse("[app]\nandorid_package = \"x.y\"\n").unwrap_err();
        assert!(
            matches!(err, ManifestError::UnknownKey { ref key } if key == "app.andorid_package")
        );
    }

    #[test]
    fn rejects_unknown_platform() {
        let err = AppConfig::parse("[[app.plugin]]\nid = \"a\"\nplatforms = [\"andriod\"]\n")
            .unwrap_err();
        assert!(matches!(err, ManifestError::InvalidValue { .. }));
    }

    #[test]
    fn rejects_bad_app_id() {
        for id in ["demo", "dev..demo", "dev.1demo", "dev.demo-app"] {
            let err = AppConfig::parse(&format!("[app]\nid = \"{id}\"\n")).unwrap_err();
            assert!(matches!(err, ManifestError::InvalidValue { .. }), "{id}");
        }
    }

    #[test]
    fn rejects_out_of_range_build() {
        for build in ["0", "-3", "2100000001"] {
            let err = AppConfig::parse(&format!("[app]\nbuild = {build}\n")).unwrap_err();
            assert!(matches!(err, ManifestError::InvalidValue { .. }), "{build}");
        }
    }

    #[test]
    fn rejects_wrong_type() {
        let err = AppConfig::parse("[app]\nauto_register = \"yes\"\n").unwrap_err();
        assert!(matches!(err, ManifestError::TypeMismatch { .. }));
    }

    fn krate() -> CrateInfo {
        CrateInfo {
            package: "my-app".to_owned(),
            version: "0.3.0".to_owned(),
            lib_name: "my_app".to_owned(),
            crate_types: vec!["cdylib".to_owned()],
            manifest_dir: PathBuf::from("/work/my-app"),
        }
    }

    #[test]
    fn metadata_falls_back_to_cargo() {
        let meta = AppMetadata::resolve(&AppIdentity::default(), &MinVersions::default(), krate());
        assert_eq!(meta.name, "my-app");
        assert_eq!(meta.version, "0.3.0");
        assert_eq!(meta.build, 1);
        assert!(meta.id.is_none());
        assert!(meta.icon.is_none());
    }

    #[test]
    fn metadata_prefers_app_section() {
        let identity = AppIdentity {
            id: Some(AppId::try_from("com.example.app").unwrap()),
            name: Some("Example".to_owned()),
            version: Some("2.0.0".to_owned()),
            build: Some(7),
            icon: Some(PathBuf::from("icon.png")),
        };
        let min = MinVersions {
            android: Some(26),
            ..MinVersions::default()
        };
        let meta = AppMetadata::resolve(&identity, &min, krate());
        assert_eq!(meta.name, "Example");
        assert_eq!(meta.version, "2.0.0");
        assert_eq!(meta.build, 7);
        assert_eq!(meta.icon, Some(PathBuf::from("/work/my-app/icon.png")));
        assert_eq!(meta.min_android, Some(26));
    }
}
