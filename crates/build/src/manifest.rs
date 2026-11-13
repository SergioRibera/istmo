//! `istmo.toml`: the [`Manifest`] a plugin or app crate declares, and
//! the types it is made of.
//!
//! Parsing lives in `parse`, `Info.plist` entries in `plist`; the
//! build-script entry points that consume a manifest are in
//! [`crate::emit`] and re-exported here.

use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::DocumentMut;
use wincode::{SchemaRead, SchemaWrite};

use crate::min_versions::MinVersions;
use crate::native_deps::NativeDeps;

mod parse;
mod plist;

pub use crate::emit::{
    ResolvedWiring, emit, emit_from, emit_manifest_metadata, emit_manifest_metadata_with_contract,
    emit_wiring_env, emit_with, resolve_wiring,
};
pub(crate) use parse::{
    IstmoToml, expect_bool, expect_integer, expect_string, expect_table, parse_android_permissions,
};
pub use plist::{InfoPlistEntry, InfoPlistValue};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, SchemaWrite, SchemaRead)]
pub enum Deployment {
    #[default]
    Local,

    Remote,
}

impl Deployment {
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            _ => None,
        }
    }
}

impl Default for PluginEntry {
    fn default() -> Self {
        Self {
            id: String::new(),
            client_type: None,
            default_deployment: Deployment::Local,
            auto_register: true,
            android_service: None,
            ios_background: None,
            info_plist: Vec::new(),
            android_package: None,
            android_backend: None,
            android_backend_arg: AndroidBackendArg::default(),
            ios_backend: None,
            android_permissions: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct PluginEntry {
    pub id: String,

    pub client_type: Option<String>,

    pub default_deployment: Deployment,

    /// Whether `emit_app` should auto-register this plugin's dispatcher
    /// in the generated `IstmoPluginRegistry`. Default `true`. Plugins
    /// with bespoke construction (custom config, non-standard
    /// `BackendImpl` signature) set this to `false` and expect the app
    /// author to register the handler manually.
    pub auto_register: bool,

    /// Optional Android `LifecycleService` shim declaration. When
    /// present, [`emit_app`](crate::emit_app()) materialises the Kotlin
    /// class and a matching `<service …/>` `AndroidManifest.xml`
    /// fragment for the app to include.
    pub android_service: Option<AndroidServiceSpec>,

    /// Optional iOS `BGTaskScheduler` (or continuous background mode)
    /// shim declaration. When present, [`emit_app`](crate::emit_app())
    /// materialises the Swift class and the `Info.plist` fragment.
    pub ios_background: Option<IosBackgroundSpec>,

    /// `Info.plist` entries the plugin requires on iOS — usage
    /// descriptions such as `NSFaceIDUsageDescription`, capability
    /// flags, … [`emit_app`](crate::emit_app()) merges them into the
    /// consuming app's `Info.plist` (between the istmo markers) and
    /// into the `Info.plist.background.xml` sidecar. Apps override
    /// individual values through `[app.info_plist]`.
    pub info_plist: Vec<InfoPlistEntry>,

    /// Kotlin package the app generates this plugin's contract sources
    /// into (dispatcher, `<T>Backend`, types, codecs). A plugin that
    /// ships `native/android/` sources sets it to the package those
    /// sources declare, so they name the generated types without an
    /// import and compile whatever `[app] android_package` the consuming
    /// app picks. `None` generates into the app's codegen package.
    pub android_package: Option<String>,

    /// Fully-qualified Kotlin class handed to the generated dispatcher
    /// by `IstmoPluginRegistry` (the backend, or the factory for
    /// stateful plugins). Defaults to `<T>BackendImpl` / `<T>FactoryImpl`
    /// in [`Self::android_package`], else in the app's codegen package.
    pub android_backend: Option<String>,

    /// What that class's constructor receives.
    pub android_backend_arg: AndroidBackendArg,

    /// Swift type handed to the generated dispatcher. Defaults to
    /// `<T>BackendImpl` / `<T>FactoryImpl`.
    pub ios_backend: Option<String>,

    /// `android.permission.*` identifiers emitted as `<uses-permission>`
    /// entries in the consuming app's `AndroidManifest.xml` when this
    /// plugin is wired through `istmo::runtime!`. The app's build.rs
    /// aggregates this list across every wired plugin (walked via
    /// `DEP_*_ISTMO_MANIFEST`) — zero manifest entries from istmo for
    /// plugins the app does not use.
    ///
    /// Plugins that offer both a zero-permission and a permission-gated
    /// surface should split into two plugin types (e.g. `NetworkOnline`
    /// vs `NetworkState`) rather than toggle inside one plugin. The
    /// wired-plugin set is the authoritative capability granularity.
    pub android_permissions: Vec<AndroidPermission>,
}

/// One `<uses-permission>` entry contributed to the consuming app's
/// `AndroidManifest.xml`.
///
/// TOML forms:
///
/// ```toml
/// # short — permission name only
/// android_permissions = ["android.permission.ACCESS_NETWORK_STATE"]
///
/// # long — inline table with optional qualifier(s)
/// android_permissions = [
///     "android.permission.ACCESS_NETWORK_STATE",
///     { name = "android.permission.WRITE_EXTERNAL_STORAGE", max_sdk_version = 28 },
/// ]
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, SchemaWrite, SchemaRead)]
pub struct AndroidPermission {
    pub name: String,
    /// Maps to `android:maxSdkVersion` — the permission is only
    /// requested on devices up to and including this API level.
    pub max_sdk_version: Option<u32>,
}

impl AndroidPermission {
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            max_sdk_version: None,
        }
    }

    #[must_use]
    pub fn with_max_sdk(name: impl Into<String>, max_sdk_version: u32) -> Self {
        Self {
            name: name.into(),
            max_sdk_version: Some(max_sdk_version),
        }
    }
}

impl From<&str> for AndroidPermission {
    fn from(name: &str) -> Self {
        Self::new(name)
    }
}

impl From<String> for AndroidPermission {
    fn from(name: String) -> Self {
        Self::new(name)
    }
}

/// Constructor argument of a plugin's Android backend, declared as
/// `android_backend_arg` in the plugin's `istmo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, SchemaWrite, SchemaRead)]
pub enum AndroidBackendArg {
    /// `android.content.Context` (`"context"`, the default).
    #[default]
    Context,
    /// The host activity, typed as the given fully-qualified `Activity`
    /// subclass. `"activity"` means `androidx.activity.ComponentActivity`.
    /// The registry skips the plugin when it is registered without an
    /// activity (e.g. from a `Service`).
    Activity(String),
}

impl AndroidBackendArg {
    const COMPONENT_ACTIVITY: &str = "androidx.activity.ComponentActivity";

    fn parse(literal: &str) -> Result<Self, ManifestError> {
        match literal {
            "context" => Ok(Self::Context),
            "activity" => Ok(Self::Activity(Self::COMPONENT_ACTIVITY.to_owned())),
            class if class.contains('.') && !class.starts_with('.') && !class.ends_with('.') => {
                Ok(Self::Activity(class.to_owned()))
            }
            other => Err(ManifestError::InvalidValue {
                key: "plugin.android_backend_arg".to_owned(),
                value: other.to_owned(),
                expected: "`context`, `activity` or a fully-qualified Activity subclass",
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct AndroidServiceSpec {
    pub class_name: String,
    pub foreground_service_type: Option<String>,
    pub exported: bool,
    pub permission: Option<String>,
    pub process: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct IosBackgroundSpec {
    pub class_name: String,
    pub task_identifier: Option<String>,
    pub kind: IosBackgroundKindSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum IosBackgroundKindSpec {
    Refresh {
        interval_minutes: u32,
    },
    Processing {
        requires_power: bool,
        requires_network: bool,
    },
    Continuous(IosContinuousModeSpec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum IosContinuousModeSpec {
    Audio,
    Location,
    Voip,
    ExternalAccessory,
    BluetoothCentral,
    BluetoothPeripheral,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct RemoteOverride {
    pub plugin: String,
    pub deployment: Deployment,
}

/// XML fragment contributed to a consuming app's `App.exe.manifest`.
///
/// Plugins that need process-wide manifest settings (DPI awareness,
/// execution level, common-controls version, …) declare fragments in
/// their `istmo.toml`; the consuming app's `build.rs` composes every
/// fragment reachable through the `DEP_*_ISTMO_MANIFEST` handover
/// into a single `.manifest` written alongside the executable (or
/// linked as a resource via `embed-resource` / `winres`).
///
/// The `name` is a stable identifier used for de-duplication when two
/// plugins declare the same fragment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, SchemaWrite, SchemaRead)]
pub struct WindowsManifestFragment {
    pub name: String,
    pub xml: String,
}

/// A single `[[assets]]` entry as declared in an `istmo.toml`.
///
/// The [`path`](AssetEntry::path) field accepts three shapes, resolved
/// at build time by [`crate::assets::resolve_assets`]:
///
/// - A single-file path (`"assets/schema.sql"`).
/// - A directory path (trailing `/` optional) — walked recursively.
/// - A glob pattern (contains `*`, `?` or `[`) — matched against the
///   crate root via `globset`.
///
/// Paths are always relative to the manifest's directory unless
/// absolute.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, SchemaWrite, SchemaRead)]
pub struct AssetEntry {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct Manifest {
    pub plugins: Vec<PluginEntry>,

    pub native_deps: NativeDeps,

    pub remote_overrides: Vec<RemoteOverride>,

    pub windows_manifest_fragments: Vec<WindowsManifestFragment>,

    /// Oldest OS releases the crate supports (`[min_versions]`). On a
    /// plugin crate these are requirements checked against every
    /// consuming app; on an app crate they are the floor the app ships
    /// to.
    pub min_versions: MinVersions,

    /// Assets declared by the crate at the top-level `[[assets]]`
    /// section. Every plugin id in this manifest can address them via
    /// `istmo::assets::plugin(id).read(...)`; the app addresses its own
    /// entries via `istmo::assets::read(...)`.
    pub assets: Vec<AssetEntry>,
}

impl Manifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let doc: DocumentMut = source.parse().map_err(ManifestError::Parse)?;
        Self::from_document(&doc)
    }

    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ManifestError> {
        let path = path.as_ref();
        let text = fs::read_to_string(path).map_err(|error| ManifestError::Io {
            path: path.to_path_buf(),
            error,
        })?;
        Self::parse(&text)
    }

    #[must_use]
    pub fn primary_id(&self) -> &str {
        &self.plugins[0].id
    }

    pub fn plugin_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.plugins.iter().map(|p| p.id.as_str())
    }

    #[must_use]
    pub fn into_native_deps(self) -> NativeDeps {
        self.native_deps
    }
}

#[derive(Debug)]
pub enum ManifestError {
    Io {
        path: PathBuf,
        error: std::io::Error,
    },

    Parse(toml_edit::TomlError),

    Missing {
        key: &'static str,
    },

    TypeMismatch {
        key: String,
        expected: &'static str,
    },

    UnknownKey {
        key: String,
    },

    UnknownGradleScope(String),

    UnknownDeployment {
        key: String,
        value: String,
    },

    UnknownBackgroundKind {
        value: String,
    },

    UnknownContinuousMode {
        value: String,
    },

    InvalidVersion {
        key: String,
        value: String,
    },

    /// A key holds a value of the right type that is not one of the
    /// accepted ones (`[app] id = "not a bundle id"`, an unknown
    /// platform name, …).
    InvalidValue {
        key: String,
        value: String,
        expected: &'static str,
    },
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io { path, error } => write!(f, "reading {}: {error}", path.display()),
            Self::Parse(err) => write!(f, "parsing istmo.toml: {err}"),
            Self::Missing { key } => write!(f, "istmo.toml missing required key `{key}`"),
            Self::TypeMismatch { key, expected } => {
                write!(f, "istmo.toml key `{key}` expected {expected}")
            }
            Self::UnknownKey { key } => write!(f, "istmo.toml unknown key `{key}`"),
            Self::UnknownGradleScope(s) => write!(
                f,
                "istmo.toml unknown gradle scope `{s}` (expected implementation, api, runtimeOnly or compileOnly)"
            ),
            Self::UnknownDeployment { key, value } => write!(
                f,
                "istmo.toml key `{key}` = `{value}` (expected `local` or `remote`)"
            ),
            Self::UnknownBackgroundKind { value } => write!(
                f,
                "istmo.toml key `plugin.ios_background.kind` = `{value}` \
                 (expected `refresh`, `processing` or `continuous`)"
            ),
            Self::UnknownContinuousMode { value } => write!(
                f,
                "istmo.toml key `plugin.ios_background.continuous_mode` = `{value}` \
                 (expected `audio`, `location`, `voip`, `external_accessory`, \
                 `bluetooth_central` or `bluetooth_peripheral`)"
            ),
            Self::InvalidVersion { key, value } => write!(
                f,
                "istmo.toml key `{key}` = `{value}` (expected an API level integer for \
                 `android`, or a `major[.minor[.patch]]` string)"
            ),
            Self::InvalidValue {
                key,
                value,
                expected,
            } => write!(
                f,
                "istmo.toml key `{key}` = `{value}` (expected {expected})"
            ),
        }
    }
}

impl std::error::Error for ManifestError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { error, .. } => Some(error),
            Self::Parse(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::native_deps::GradleScope;

    const FULL_MANIFEST: &str = r#"
[plugin]
id = "istmo.google_sign_in"

[[gradle]]
scope = "implementation"
group = "androidx.credentials"
artifact = "credentials"
version = "1.3.0"

[[gradle]]
group = "androidx.credentials"
artifact = "credentials-play-services-auth"
version = "1.3.0"

[[gradle]]
scope = "api"
group = "com.google.android.libraries.identity.googleid"
artifact = "googleid"
version = "1.1.1"

[[swift_package]]
url = "https://github.com/google/GoogleSignIn-iOS.git"
product = "GoogleSignIn"
from_version = "7.0.0"
"#;

    const MULTI_PLUGIN_MANIFEST: &str = r#"
[[plugin]]
id = "istmo.google_sign_in"
client_type = "::istmo_plugins::SignInClient"

  [[plugin.gradle]]
  group = "androidx.credentials"
  artifact = "credentials"
  version = "1.3.0"

  [[plugin.swift_package]]
  url = "https://github.com/google/GoogleSignIn-iOS.git"
  product = "GoogleSignIn"
  from_version = "7.0.0"

[[plugin]]
id = "istmo.admob"

  [[plugin.gradle]]
  group = "com.google.android.gms"
  artifact = "play-services-ads"
  version = "23.0.0"

# Shared across every plugin in this crate.
[[gradle]]
group = "androidx.core"
artifact = "core-ktx"
version = "1.13.0"
"#;

    #[test]
    fn parses_full_manifest_with_defaults() {
        let m = Manifest::parse(FULL_MANIFEST).expect("parse");
        assert_eq!(m.plugins.len(), 1);
        assert_eq!(m.primary_id(), "istmo.google_sign_in");
        assert!(m.plugins[0].client_type.is_none());
        let gradle: Vec<_> = m.native_deps.gradle_entries().collect();
        assert_eq!(gradle.len(), 3);

        let default_scope = gradle
            .iter()
            .find(|d| d.coord.artifact == "credentials-play-services-auth")
            .expect("entry present");
        assert_eq!(default_scope.scope, GradleScope::Implementation);
        let swift: Vec<_> = m.native_deps.swift_entries().collect();
        assert_eq!(swift.len(), 1);
        assert_eq!(swift[0].product, "GoogleSignIn");
        assert_eq!(swift[0].from_version, "7.0.0");
    }

    #[test]
    fn parses_multi_plugin_manifest_with_nested_and_shared_deps() {
        let m = Manifest::parse(MULTI_PLUGIN_MANIFEST).expect("parse");
        let ids: Vec<_> = m.plugin_ids().collect();
        assert_eq!(ids, vec!["istmo.google_sign_in", "istmo.admob"]);
        assert_eq!(
            m.plugins[0].client_type.as_deref(),
            Some("::istmo_plugins::SignInClient"),
        );
        assert!(m.plugins[1].client_type.is_none());

        let gradle: Vec<_> = m.native_deps.gradle_entries().collect();
        assert_eq!(gradle.len(), 3);
        assert!(gradle.iter().any(|g| g.coord.artifact == "credentials"));
        assert!(
            gradle
                .iter()
                .any(|g| g.coord.artifact == "play-services-ads")
        );
        assert!(gradle.iter().any(|g| g.coord.artifact == "core-ktx"));
        assert_eq!(m.native_deps.swift_entries().count(), 1);
    }

    #[test]
    fn manifest_with_only_shared_gradle_is_valid() {
        let m = Manifest::parse("[[gradle]]\ngroup=\"g\"\nartifact=\"a\"\nversion=\"1\"\n")
            .expect("parse");
        assert_eq!(m.plugins, []);
        assert_eq!(m.native_deps.gradle_entries().count(), 1);
    }

    #[test]
    fn manifest_with_only_app_section_is_valid() {
        let m = Manifest::parse("[app]\nandroid = false\n").expect("parse");
        assert_eq!(m.plugins, []);
        assert_eq!(m.remote_overrides, []);
    }

    #[test]
    fn missing_plugin_id_is_reported() {
        let err = Manifest::parse("[plugin]\n").expect_err("must fail");
        assert!(matches!(err, ManifestError::Missing { key: "plugin.id" }));
    }

    #[test]
    fn missing_plugin_id_in_multi_form_is_reported() {
        let src = r#"
[[plugin]]
id = "istmo.a"

[[plugin]]
client_type = "::foo::Bar"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        assert!(matches!(err, ManifestError::Missing { key: "plugin.id" }));
    }

    fn invalid_value_key(src: &str) -> String {
        match Manifest::parse(src).expect_err("must fail") {
            ManifestError::InvalidValue { key, .. } => key,
            other => panic!("expected InvalidValue, got {other:?}"),
        }
    }

    #[test]
    fn empty_plugin_id_is_rejected() {
        assert_eq!(invalid_value_key("[plugin]\nid = \"\"\n"), "plugin.id");
    }

    #[test]
    fn plugin_id_outside_the_portable_charset_is_rejected() {
        for id in [
            "istmo:camera",
            "istmo camera",
            "istmo/camera",
            "kamera\u{e9}",
        ] {
            let src = format!("[plugin]\nid = \"{id}\"\n");
            assert_eq!(invalid_value_key(&src), "plugin.id", "{id}");
        }
        assert!(Manifest::parse("[plugin]\nid = \"Istmo.data_store-2\"\n").is_ok());
    }

    #[test]
    fn malformed_client_type_is_rejected() {
        let src = "[plugin]\nid = \"istmo.a\"\nclient_type = \"::foo::\"\n";
        assert_eq!(invalid_value_key(src), "plugin.client_type");
        let src = "[plugin]\nid = \"istmo.a\"\nclient_type = \"foo bar\"\n";
        assert_eq!(invalid_value_key(src), "plugin.client_type");
    }

    #[test]
    fn duplicate_plugin_ids_are_rejected() {
        let src = "[[plugin]]\nid = \"istmo.a\"\n\n[[plugin]]\nid = \"istmo.a\"\n";
        let err = Manifest::parse(src).expect_err("must fail");
        assert!(
            matches!(&err, ManifestError::InvalidValue { key, value, .. }
                if key == "plugin.id" && value == "istmo.a"),
            "{err:?}"
        );
    }

    #[test]
    fn istmo_toml_reads_manifest_and_app_section_once() {
        let dir = std::env::temp_dir().join(format!("istmo-toml-read-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("istmo.toml");
        assert!(IstmoToml::read(&path).unwrap().manifest.is_none());
        fs::write(
            &path,
            "[plugin]\nid = \"istmo.a\"\n\n[app]\nandroid = false\n",
        )
        .unwrap();
        let toml = IstmoToml::read(&path).unwrap();
        assert_eq!(toml.manifest.unwrap().primary_id(), "istmo.a");
        assert_eq!(toml.app.android, Some(false));
        fs::write(&path, "[plugin]\nid = \"\"\n").unwrap();
        assert!(IstmoToml::read(&path).is_err());
    }

    #[test]
    fn unknown_top_level_key_is_reported() {
        let src = r#"
[plugin]
id = "istmo.example"

[[widgets]]
name = "foo"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownKey { key } => assert_eq!(key, "widgets"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unknown_plugin_entry_key_is_reported() {
        let src = r#"
[[plugin]]
id = "istmo.example"
foo = "bar"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownKey { key } => assert_eq!(key, "plugin.foo"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unknown_gradle_scope_is_reported() {
        let src = r#"
[plugin]
id = "istmo.example"

[[gradle]]
scope = "runtime"
group = "g"
artifact = "a"
version = "1"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownGradleScope(s) => assert_eq!(s, "runtime"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn unknown_gradle_key_is_reported() {
        let src = r#"
[plugin]
id = "istmo.example"

[[gradle]]
group = "g"
artifact = "a"
version = "1"
extra = "boom"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownKey { key } => assert_eq!(key, "gradle.extra"),
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn type_mismatch_on_non_string_value() {
        let src = r#"
[plugin]
id = "istmo.example"

[[gradle]]
group = 42
artifact = "a"
version = "1"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::TypeMismatch { key, expected } => {
                assert_eq!(key, "gradle.group");
                assert_eq!(expected, "string");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    }

    #[test]
    fn empty_native_deps_yields_empty_bundle() {
        let src = r#"
[plugin]
id = "istmo.example"
"#;
        let m = Manifest::parse(src).expect("parse");
        assert!(m.native_deps.is_empty());
    }

    #[test]
    fn manifest_native_deps_survive_wire_round_trip() {
        let m = Manifest::parse(FULL_MANIFEST).expect("parse");
        let hex = crate::serialize_native_deps(&m.native_deps).expect("serialize");
        let back = crate::deserialize_native_deps(&hex).expect("deserialize");
        assert_eq!(back, m.native_deps);
    }

    #[test]
    fn parses_android_service_spec() {
        let src = r#"
[plugin]
id = "myapp.sync"

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"
exported = false
permission = "android.permission.FOREGROUND_SERVICE_DATA_SYNC"
"#;
        let m = Manifest::parse(src).expect("parse");
        let spec = m.plugins[0]
            .android_service
            .as_ref()
            .expect("android_service parsed");
        assert_eq!(spec.class_name, "SyncForegroundService");
        assert_eq!(spec.foreground_service_type.as_deref(), Some("dataSync"));
        assert!(!spec.exported);
        assert_eq!(
            spec.permission.as_deref(),
            Some("android.permission.FOREGROUND_SERVICE_DATA_SYNC"),
        );
        assert!(spec.process.is_none());
    }

    #[test]
    fn parses_ios_background_refresh_spec() {
        let src = r#"
[plugin]
id = "myapp.sync"

[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"
interval_minutes = 15
"#;
        let m = Manifest::parse(src).expect("parse");
        let spec = m.plugins[0]
            .ios_background
            .as_ref()
            .expect("ios_background parsed");
        assert_eq!(spec.class_name, "SyncBackgroundHandler");
        assert_eq!(
            spec.task_identifier.as_deref(),
            Some("com.myapp.sync.refresh")
        );
        assert!(matches!(
            spec.kind,
            IosBackgroundKindSpec::Refresh {
                interval_minutes: 15
            }
        ));
    }

    #[test]
    fn parses_ios_background_processing_spec() {
        let src = r#"
[plugin]
id = "myapp.crunch"

[plugin.ios_background]
class_name = "CrunchBackground"
task_identifier = "com.myapp.crunch"
kind = "processing"
requires_power = true
requires_network = true
"#;
        let m = Manifest::parse(src).expect("parse");
        let kind = &m.plugins[0].ios_background.as_ref().expect("parsed").kind;
        assert!(matches!(
            kind,
            IosBackgroundKindSpec::Processing {
                requires_power: true,
                requires_network: true,
            }
        ));
    }

    #[test]
    fn parses_ios_background_continuous_spec() {
        let src = r#"
[plugin]
id = "myapp.player"

[plugin.ios_background]
class_name = "PlayerBackground"
kind = "continuous"
continuous_mode = "audio"
"#;
        let m = Manifest::parse(src).expect("parse");
        let kind = &m.plugins[0].ios_background.as_ref().expect("parsed").kind;
        assert!(matches!(
            kind,
            IosBackgroundKindSpec::Continuous(IosContinuousModeSpec::Audio)
        ));
    }

    #[test]
    fn unknown_ios_background_kind_reported() {
        let src = r#"
[plugin]
id = "myapp.sync"

[plugin.ios_background]
class_name = "Bg"
kind = "bogus"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownBackgroundKind { value } => assert_eq!(value, "bogus"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn refresh_kind_requires_interval() {
        let src = r#"
[plugin]
id = "myapp.sync"

[plugin.ios_background]
class_name = "Bg"
kind = "refresh"
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        assert!(matches!(
            err,
            ManifestError::Missing {
                key: "plugin.ios_background.interval_minutes"
            }
        ));
    }

    #[test]
    fn parses_top_level_assets_section() {
        let src = r#"
[plugin]
id = "istmo.example"

[[assets]]
path = "assets/img/"

[[assets]]
path = "assets/schema.sql"

[[assets]]
path = "assets/**/*.png"
"#;
        let m = Manifest::parse(src).expect("parse");
        assert_eq!(m.assets.len(), 3);
        assert_eq!(m.assets[0].path, "assets/img/");
        assert_eq!(m.assets[1].path, "assets/schema.sql");
        assert_eq!(m.assets[2].path, "assets/**/*.png");
    }

    #[test]
    fn unknown_asset_key_is_reported() {
        let src = r#"
[[assets]]
path = "x"
weight = 3
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::UnknownKey { key } => assert_eq!(key, "assets.weight"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn empty_asset_path_is_rejected() {
        let src = r#"
[[assets]]
path = ""
"#;
        let err = Manifest::parse(src).expect_err("must fail");
        match err {
            ManifestError::InvalidValue { key, .. } => assert_eq!(key, "assets.path"),
            other => panic!("unexpected: {other:?}"),
        }
    }

    #[test]
    fn service_and_background_survive_manifest_round_trip() {
        let src = r#"
[plugin]
id = "myapp.sync"
client_type = "::myapp::SyncServiceClient"

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"

[plugin.ios_background]
class_name = "SyncBg"
task_identifier = "com.myapp.sync"
kind = "refresh"
interval_minutes = 30
"#;
        let m = Manifest::parse(src).expect("parse");
        let hex = crate::serialize_manifest(&m).expect("serialize");
        let back = crate::deserialize_manifest(&hex).expect("deserialize");
        assert_eq!(back, m);
        assert!(back.plugins[0].android_service.is_some());
        assert!(back.plugins[0].ios_background.is_some());
    }
}
