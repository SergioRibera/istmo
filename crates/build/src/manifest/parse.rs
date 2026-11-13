//! `istmo.toml` → [`Manifest`]: a `toml_edit` DOM walk that rejects
//! unknown keys eagerly and validates every value it keeps.

use std::fs;
use std::path::Path;

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use super::{
    AndroidBackendArg, AndroidPermission, AndroidServiceSpec, AssetEntry, Deployment,
    InfoPlistEntry, IosBackgroundKindSpec, IosBackgroundSpec, IosContinuousModeSpec, Manifest,
    ManifestError, PluginEntry, RemoteOverride, WindowsManifestFragment,
};
use crate::min_versions::{MinVersions, OsVersion};
use crate::native_deps::{GradleCoord, GradleDep, GradleScope, NativeDeps, SwiftPackageDep};

const KNOWN_KEYS: &[&str] = &[
    "plugin",
    "gradle",
    "swift_package",
    "remote_override",
    "windows_manifest_fragment",
    "min_versions",
    "app",
    "assets",
];

const KNOWN_ASSET_KEYS: &[&str] = &["path"];

const KNOWN_MIN_VERSION_KEYS: &[&str] = &["android", "ios", "macos", "windows"];

const KNOWN_WINDOWS_MANIFEST_KEYS: &[&str] = &["name", "xml"];

const KNOWN_PLUGIN_KEYS: &[&str] = &[
    "id",
    "client_type",
    "default_deployment",
    "auto_register",
    "gradle",
    "swift_package",
    "android_service",
    "ios_background",
    "info_plist",
    "android_permissions",
    "android_package",
    "android_backend",
    "android_backend_arg",
    "ios_backend",
];

const KNOWN_ANDROID_SERVICE_KEYS: &[&str] = &[
    "class_name",
    "foreground_service_type",
    "exported",
    "permission",
    "process",
];

const KNOWN_IOS_BACKGROUND_KEYS: &[&str] = &[
    "class_name",
    "task_identifier",
    "kind",
    "interval_minutes",
    "requires_power",
    "requires_network",
    "continuous_mode",
];

const KNOWN_OVERRIDE_KEYS: &[&str] = &["plugin", "deployment"];

impl Manifest {
    pub(crate) fn from_document(doc: &DocumentMut) -> Result<Self, ManifestError> {
        for (name, _) in doc.as_table() {
            if !KNOWN_KEYS.contains(&name) {
                return Err(ManifestError::UnknownKey {
                    key: name.to_owned(),
                });
            }
        }
        let mut native_deps = NativeDeps::new();
        let plugins = parse_plugins(doc, &mut native_deps)?;
        if let Some(item) = doc.get("gradle") {
            for entry in expect_array_of_tables(item, "gradle")? {
                native_deps.add_gradle(&parse_gradle(entry, "gradle")?);
            }
        }
        if let Some(item) = doc.get("swift_package") {
            for entry in expect_array_of_tables(item, "swift_package")? {
                native_deps.add_swift_package(&parse_swift_package(entry, "swift_package")?);
            }
        }
        let mut remote_overrides = Vec::new();
        if let Some(item) = doc.get("remote_override") {
            for entry in expect_array_of_tables(item, "remote_override")? {
                remote_overrides.push(parse_remote_override(entry)?);
            }
        }
        let mut windows_manifest_fragments = Vec::new();
        if let Some(item) = doc.get("windows_manifest_fragment") {
            for entry in expect_array_of_tables(item, "windows_manifest_fragment")? {
                windows_manifest_fragments.push(parse_windows_manifest_fragment(entry)?);
            }
        }
        let min_versions = match doc.get("min_versions") {
            Some(item) => parse_min_versions(expect_table(item, "min_versions")?)?,
            None => MinVersions::default(),
        };
        let mut assets = Vec::new();
        if let Some(item) = doc.get("assets") {
            for entry in expect_array_of_tables(item, "assets")? {
                assets.push(parse_asset_entry(entry, "assets")?);
            }
        }
        Ok(Self {
            plugins,
            native_deps,
            remote_overrides,
            windows_manifest_fragments,
            min_versions,
            assets,
        })
    }
}

/// A crate's `istmo.toml`, read and parsed once per build-script run
/// and handed to every emit step.
#[derive(Debug, Clone, Default)]
pub(crate) struct IstmoToml {
    /// `None` when the crate has no `istmo.toml`.
    pub(crate) manifest: Option<Manifest>,
    /// The `[app]` section; empty without one.
    pub(crate) app: crate::app_config::AppConfig,
}

impl IstmoToml {
    /// Read `path`. A missing file is an empty configuration.
    pub(crate) fn read(path: &Path) -> Result<Self, ManifestError> {
        let text = match fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(error) => {
                return Err(ManifestError::Io {
                    path: path.to_path_buf(),
                    error,
                });
            }
        };
        let doc: DocumentMut = text.parse().map_err(ManifestError::Parse)?;
        Ok(Self {
            manifest: Some(Manifest::from_document(&doc)?),
            app: crate::app_config::AppConfig::from_document(&doc)?,
        })
    }

    /// [`Self::read`] for a build script: a malformed manifest is a user
    /// error, so it fails the build naming the file.
    pub(crate) fn load(path: &Path) -> Self {
        Self::read(path).unwrap_or_else(|err| panic!("istmo-build: {}: {err}", path.display()))
    }
}

fn parse_plugins(
    doc: &DocumentMut,
    native_deps: &mut NativeDeps,
) -> Result<Vec<PluginEntry>, ManifestError> {
    let Some(item) = doc.get("plugin") else {
        return Ok(Vec::new());
    };
    if let Some(table) = item.as_table() {
        let entry = parse_plugin_entry(table, "plugin", native_deps)?;
        Ok(vec![entry])
    } else if let Some(array) = item.as_array_of_tables() {
        if array.is_empty() {
            return Err(ManifestError::Missing { key: "plugin" });
        }
        let mut entries: Vec<PluginEntry> = Vec::with_capacity(array.len());
        for table in array {
            let entry = parse_plugin_entry(table, "plugin", native_deps)?;
            if entries.iter().any(|seen| seen.id == entry.id) {
                return Err(ManifestError::InvalidValue {
                    key: "plugin.id".to_owned(),
                    value: entry.id,
                    expected: "an id not already used by another [[plugin]] entry",
                });
            }
            entries.push(entry);
        }
        Ok(entries)
    } else {
        Err(ManifestError::TypeMismatch {
            key: "plugin".to_owned(),
            expected: "table ([plugin]) or array of tables ([[plugin]])",
        })
    }
}

fn parse_plugin_entry(
    table: &Table,
    context: &'static str,
    native_deps: &mut NativeDeps,
) -> Result<PluginEntry, ManifestError> {
    for (name, _) in table {
        if !KNOWN_PLUGIN_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("{context}.{name}"),
            });
        }
    }
    let id_item = table
        .get("id")
        .ok_or(ManifestError::Missing { key: "plugin.id" })?;
    let id = expect_string(id_item, "plugin.id")?.to_owned();
    if !is_valid_plugin_id(&id) {
        return Err(ManifestError::InvalidValue {
            key: "plugin.id".to_owned(),
            value: id,
            expected: "a non-empty id of ASCII letters, digits, `.`, `_` or `-`",
        });
    }
    let client_type = table
        .get("client_type")
        .map(parse_client_type)
        .transpose()?;
    let default_deployment = match table.get("default_deployment") {
        Some(item) => {
            let literal = expect_string(item, "plugin.default_deployment")?;
            Deployment::parse(literal).ok_or_else(|| ManifestError::UnknownDeployment {
                key: "plugin.default_deployment".to_owned(),
                value: literal.to_owned(),
            })?
        }
        None => Deployment::Local,
    };
    let auto_register = match table.get("auto_register") {
        Some(item) => expect_bool(item, "plugin.auto_register")?,
        None => true,
    };
    if let Some(item) = table.get("gradle") {
        for entry in expect_array_of_tables(item, "plugin.gradle")? {
            native_deps.add_gradle(&parse_gradle(entry, "plugin.gradle")?);
        }
    }
    if let Some(item) = table.get("swift_package") {
        for entry in expect_array_of_tables(item, "plugin.swift_package")? {
            native_deps.add_swift_package(&parse_swift_package(entry, "plugin.swift_package")?);
        }
    }
    let android_service = match table.get("android_service") {
        Some(item) => Some(parse_android_service(expect_table(
            item,
            "plugin.android_service",
        )?)?),
        None => None,
    };
    let ios_background = match table.get("ios_background") {
        Some(item) => Some(parse_ios_background(expect_table(
            item,
            "plugin.ios_background",
        )?)?),
        None => None,
    };
    let info_plist = match table.get("info_plist") {
        Some(item) => InfoPlistEntry::parse_table(
            expect_table(item, "plugin.info_plist")?,
            "plugin.info_plist",
        )?,
        None => Vec::new(),
    };
    let android_permissions = match table.get("android_permissions") {
        Some(item) => parse_android_permissions(item, "plugin.android_permissions")?,
        None => Vec::new(),
    };
    let optional_string = |key: &'static str, full: &'static str| {
        table
            .get(key)
            .map(|item| expect_string(item, full).map(str::to_owned))
            .transpose()
    };
    let android_backend_arg = match table.get("android_backend_arg") {
        Some(item) => AndroidBackendArg::parse(expect_string(item, "plugin.android_backend_arg")?)?,
        None => AndroidBackendArg::default(),
    };
    Ok(PluginEntry {
        id,
        client_type,
        default_deployment,
        auto_register,
        android_service,
        ios_background,
        info_plist,
        android_package: table
            .get("android_package")
            .map(|item| parse_kotlin_package(item, "plugin.android_package"))
            .transpose()?,
        android_backend: optional_string("android_backend", "plugin.android_backend")?,
        android_backend_arg,
        ios_backend: optional_string("ios_backend", "plugin.ios_backend")?,
        android_permissions,
    })
}

/// Plugin ids end up in asset directory names, Kotlin/Swift registry
/// keys and env-var payloads, so they stay within a portable charset.
fn is_valid_plugin_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A dotted Kotlin package name: every segment a plain identifier, so it
/// is both a valid `package` line and a source directory path.
fn parse_kotlin_package(item: &Item, key: &'static str) -> Result<String, ManifestError> {
    let literal = expect_string(item, key)?;
    let is_segment = |segment: &str| {
        segment
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && segment
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_')
    };
    if !literal.split('.').all(is_segment) {
        return Err(ManifestError::InvalidValue {
            key: key.to_owned(),
            value: literal.to_owned(),
            expected: "a Kotlin package such as `dev.istmo.plugins.share`",
        });
    }
    Ok(literal.to_owned())
}

/// `istmo::runtime!` parses every auto-wired client type as a path;
/// rejecting a malformed one here names the manifest instead.
fn parse_client_type(item: &Item) -> Result<String, ManifestError> {
    let literal = expect_string(item, "plugin.client_type")?;
    if syn::parse_str::<syn::Path>(literal).is_err() {
        return Err(ManifestError::InvalidValue {
            key: "plugin.client_type".to_owned(),
            value: literal.to_owned(),
            expected: "a Rust type path such as `::my_crate::MyClient`",
        });
    }
    Ok(literal.to_owned())
}

fn parse_android_service(
    table: &dyn toml_edit::TableLike,
) -> Result<AndroidServiceSpec, ManifestError> {
    for (name, _) in table.iter() {
        if !KNOWN_ANDROID_SERVICE_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("plugin.android_service.{name}"),
            });
        }
    }
    let class_name = table
        .get("class_name")
        .ok_or(ManifestError::Missing {
            key: "plugin.android_service.class_name",
        })
        .and_then(|item| expect_string(item, "plugin.android_service.class_name"))?
        .to_owned();
    let foreground_service_type = match table.get("foreground_service_type") {
        Some(item) => {
            Some(expect_string(item, "plugin.android_service.foreground_service_type")?.to_owned())
        }
        None => None,
    };
    let exported = match table.get("exported") {
        Some(item) => expect_bool(item, "plugin.android_service.exported")?,
        None => false,
    };
    let permission = match table.get("permission") {
        Some(item) => Some(expect_string(item, "plugin.android_service.permission")?.to_owned()),
        None => None,
    };
    let process = match table.get("process") {
        Some(item) => Some(expect_string(item, "plugin.android_service.process")?.to_owned()),
        None => None,
    };
    Ok(AndroidServiceSpec {
        class_name,
        foreground_service_type,
        exported,
        permission,
        process,
    })
}

fn parse_ios_background(
    table: &dyn toml_edit::TableLike,
) -> Result<IosBackgroundSpec, ManifestError> {
    for (name, _) in table.iter() {
        if !KNOWN_IOS_BACKGROUND_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("plugin.ios_background.{name}"),
            });
        }
    }
    let class_name = table
        .get("class_name")
        .ok_or(ManifestError::Missing {
            key: "plugin.ios_background.class_name",
        })
        .and_then(|item| expect_string(item, "plugin.ios_background.class_name"))?
        .to_owned();
    let task_identifier = match table.get("task_identifier") {
        Some(item) => {
            Some(expect_string(item, "plugin.ios_background.task_identifier")?.to_owned())
        }
        None => None,
    };
    let kind_literal = table
        .get("kind")
        .ok_or(ManifestError::Missing {
            key: "plugin.ios_background.kind",
        })
        .and_then(|item| expect_string(item, "plugin.ios_background.kind"))?;
    let kind = match kind_literal {
        "refresh" => {
            let interval = match table.get("interval_minutes") {
                Some(item) => expect_integer(item, "plugin.ios_background.interval_minutes")?,
                None => {
                    return Err(ManifestError::Missing {
                        key: "plugin.ios_background.interval_minutes",
                    });
                }
            };
            IosBackgroundKindSpec::Refresh {
                interval_minutes: u32::try_from(interval).unwrap_or(0),
            }
        }
        "processing" => {
            let requires_power = match table.get("requires_power") {
                Some(item) => expect_bool(item, "plugin.ios_background.requires_power")?,
                None => false,
            };
            let requires_network = match table.get("requires_network") {
                Some(item) => expect_bool(item, "plugin.ios_background.requires_network")?,
                None => false,
            };
            IosBackgroundKindSpec::Processing {
                requires_power,
                requires_network,
            }
        }
        "continuous" => {
            let mode_literal = table
                .get("continuous_mode")
                .ok_or(ManifestError::Missing {
                    key: "plugin.ios_background.continuous_mode",
                })
                .and_then(|item| expect_string(item, "plugin.ios_background.continuous_mode"))?;
            let mode = match mode_literal {
                "audio" => IosContinuousModeSpec::Audio,
                "location" => IosContinuousModeSpec::Location,
                "voip" => IosContinuousModeSpec::Voip,
                "external_accessory" => IosContinuousModeSpec::ExternalAccessory,
                "bluetooth_central" => IosContinuousModeSpec::BluetoothCentral,
                "bluetooth_peripheral" => IosContinuousModeSpec::BluetoothPeripheral,
                other => {
                    return Err(ManifestError::UnknownContinuousMode {
                        value: other.to_owned(),
                    });
                }
            };
            IosBackgroundKindSpec::Continuous(mode)
        }
        other => {
            return Err(ManifestError::UnknownBackgroundKind {
                value: other.to_owned(),
            });
        }
    };
    Ok(IosBackgroundSpec {
        class_name,
        task_identifier,
        kind,
    })
}

fn parse_min_versions(table: &dyn toml_edit::TableLike) -> Result<MinVersions, ManifestError> {
    for (name, _) in table.iter() {
        if !KNOWN_MIN_VERSION_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("min_versions.{name}"),
            });
        }
    }
    let android = match table.get("android") {
        Some(item) => {
            let api = expect_integer(item, "min_versions.android")?;
            Some(
                u32::try_from(api).map_err(|_| ManifestError::InvalidVersion {
                    key: "min_versions.android".to_owned(),
                    value: api.to_string(),
                })?,
            )
        }
        None => None,
    };
    let os_version = |key: &'static str| -> Result<Option<OsVersion>, ManifestError> {
        let Some(item) = table.get(key) else {
            return Ok(None);
        };
        let full_key = format!("min_versions.{key}");
        let literal = match item {
            Item::Value(Value::String(s)) => s.value().clone(),
            Item::Value(Value::Integer(i)) => i.value().to_string(),
            _ => {
                return Err(ManifestError::TypeMismatch {
                    key: full_key,
                    expected: "version string",
                });
            }
        };
        literal
            .parse()
            .map(Some)
            .map_err(|_| ManifestError::InvalidVersion {
                key: full_key,
                value: literal,
            })
    };
    Ok(MinVersions {
        android,
        ios: os_version("ios")?,
        macos: os_version("macos")?,
        windows: os_version("windows")?,
    })
}

fn parse_windows_manifest_fragment(
    table: &Table,
) -> Result<WindowsManifestFragment, ManifestError> {
    for (name, _) in table {
        if !KNOWN_WINDOWS_MANIFEST_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("windows_manifest_fragment.{name}"),
            });
        }
    }
    let name = table
        .get("name")
        .ok_or(ManifestError::Missing {
            key: "windows_manifest_fragment.name",
        })
        .and_then(|item| expect_string(item, "windows_manifest_fragment.name"))?
        .to_owned();
    let xml = table
        .get("xml")
        .ok_or(ManifestError::Missing {
            key: "windows_manifest_fragment.xml",
        })
        .and_then(|item| expect_string(item, "windows_manifest_fragment.xml"))?
        .to_owned();
    Ok(WindowsManifestFragment { name, xml })
}

fn parse_asset_entry(table: &Table, context: &'static str) -> Result<AssetEntry, ManifestError> {
    for (name, _) in table {
        if !KNOWN_ASSET_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("{context}.{name}"),
            });
        }
    }
    let path_item = table
        .get("path")
        .ok_or(ManifestError::Missing { key: "assets.path" })?;
    let path = expect_string(path_item, "assets.path")?.to_owned();
    if path.is_empty() {
        return Err(ManifestError::InvalidValue {
            key: "assets.path".to_owned(),
            value: path,
            expected: "a non-empty path, directory or glob",
        });
    }
    Ok(AssetEntry { path })
}

fn parse_remote_override(table: &Table) -> Result<RemoteOverride, ManifestError> {
    for (name, _) in table {
        if !KNOWN_OVERRIDE_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("remote_override.{name}"),
            });
        }
    }
    let plugin_item = table.get("plugin").ok_or(ManifestError::Missing {
        key: "remote_override.plugin",
    })?;
    let plugin = expect_string(plugin_item, "remote_override.plugin")?.to_owned();
    let deployment_item = table.get("deployment").ok_or(ManifestError::Missing {
        key: "remote_override.deployment",
    })?;
    let literal = expect_string(deployment_item, "remote_override.deployment")?;
    let deployment =
        Deployment::parse(literal).ok_or_else(|| ManifestError::UnknownDeployment {
            key: "remote_override.deployment".to_owned(),
            value: literal.to_owned(),
        })?;
    Ok(RemoteOverride { plugin, deployment })
}

fn parse_gradle(table: &Table, context: &'static str) -> Result<GradleDep, ManifestError> {
    let mut scope = GradleScope::Implementation;
    let mut group: Option<&str> = None;
    let mut artifact: Option<&str> = None;
    let mut version: Option<&str> = None;
    for (name, item) in table {
        match name {
            "scope" => {
                scope = parse_gradle_scope(expect_string_ctx(item, context, "scope")?)?;
            }
            "group" => group = Some(expect_string_ctx(item, context, "group")?),
            "artifact" => artifact = Some(expect_string_ctx(item, context, "artifact")?),
            "version" => version = Some(expect_string_ctx(item, context, "version")?),
            other => {
                return Err(ManifestError::UnknownKey {
                    key: format!("{context}.{other}"),
                });
            }
        }
    }
    Ok(GradleDep::new(
        scope,
        GradleCoord::new(
            group.ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "group"),
            })?,
            artifact.ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "artifact"),
            })?,
            version.ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "version"),
            })?,
        ),
    ))
}

fn parse_gradle_scope(literal: &str) -> Result<GradleScope, ManifestError> {
    match literal {
        "implementation" => Ok(GradleScope::Implementation),
        "api" => Ok(GradleScope::Api),
        "runtimeOnly" => Ok(GradleScope::RuntimeOnly),
        "compileOnly" => Ok(GradleScope::CompileOnly),
        other => Err(ManifestError::UnknownGradleScope(other.to_owned())),
    }
}

fn parse_swift_package(
    table: &Table,
    context: &'static str,
) -> Result<SwiftPackageDep, ManifestError> {
    let mut url: Option<&str> = None;
    let mut product: Option<&str> = None;
    let mut from_version: Option<&str> = None;
    for (name, item) in table {
        match name {
            "url" => url = Some(expect_string_ctx(item, context, "url")?),
            "product" => product = Some(expect_string_ctx(item, context, "product")?),
            "from_version" => {
                from_version = Some(expect_string_ctx(item, context, "from_version")?);
            }
            other => {
                return Err(ManifestError::UnknownKey {
                    key: format!("{context}.{other}"),
                });
            }
        }
    }
    Ok(SwiftPackageDep {
        url: url
            .ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "url"),
            })?
            .to_owned(),
        product: product
            .ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "product"),
            })?
            .to_owned(),
        from_version: from_version
            .ok_or_else(|| ManifestError::Missing {
                key: static_field(context, "from_version"),
            })?
            .to_owned(),
    })
}

pub(crate) fn expect_array_of_tables<'a>(
    item: &'a Item,
    key: &'static str,
) -> Result<&'a ArrayOfTables, ManifestError> {
    item.as_array_of_tables()
        .ok_or_else(|| ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "array of tables ([[…]])",
        })
}

const KNOWN_PERMISSION_KEYS: &[&str] = &["name", "max_sdk_version"];

const RECOGNIZED_PERMISSION_PREFIXES: &[&str] = &[
    "android.permission.",
    "com.android.vending.",
    "com.google.android.",
];

pub(crate) fn parse_android_permissions(
    item: &Item,
    key: &'static str,
) -> Result<Vec<AndroidPermission>, ManifestError> {
    let array = item.as_array().ok_or_else(|| ManifestError::TypeMismatch {
        key: key.to_owned(),
        expected: "array of strings or inline tables ({ name, max_sdk_version })",
    })?;
    let mut out = Vec::with_capacity(array.len());
    for value in array {
        let perm = match value {
            Value::String(s) => AndroidPermission::new(s.value()),
            Value::InlineTable(table) => {
                for (ident, _) in table {
                    if !KNOWN_PERMISSION_KEYS.contains(&ident) {
                        return Err(ManifestError::UnknownKey {
                            key: format!("{key}.{ident}"),
                        });
                    }
                }
                let name_val = table.get("name").ok_or(ManifestError::Missing {
                    key: "plugin.android_permissions.name",
                })?;
                let name = match name_val {
                    Value::String(s) => s.value().clone(),
                    _ => {
                        return Err(ManifestError::TypeMismatch {
                            key: format!("{key}.name"),
                            expected: "string",
                        });
                    }
                };
                let max_sdk_version = match table.get("max_sdk_version") {
                    None => None,
                    Some(Value::Integer(n)) => {
                        let raw = *n.value();
                        if raw < 0 {
                            return Err(ManifestError::TypeMismatch {
                                key: format!("{key}.max_sdk_version"),
                                expected: "non-negative integer",
                            });
                        }
                        Some(u32::try_from(raw).map_err(|_| ManifestError::TypeMismatch {
                            key: format!("{key}.max_sdk_version"),
                            expected: "non-negative integer",
                        })?)
                    }
                    _ => {
                        return Err(ManifestError::TypeMismatch {
                            key: format!("{key}.max_sdk_version"),
                            expected: "non-negative integer",
                        });
                    }
                };
                AndroidPermission {
                    name,
                    max_sdk_version,
                }
            }
            _ => {
                return Err(ManifestError::TypeMismatch {
                    key: key.to_owned(),
                    expected: "string or inline table",
                });
            }
        };
        if !RECOGNIZED_PERMISSION_PREFIXES
            .iter()
            .any(|prefix| perm.name.starts_with(prefix))
        {
            println!(
                "cargo::warning=istmo-build: {key} entry `{}` uses an unrecognised prefix \
                 (expected `android.permission.*`, `com.android.vending.*` or `com.google.android.*`)",
                perm.name
            );
        }
        out.push(perm);
    }
    Ok(out)
}

pub(crate) fn expect_string<'a>(
    item: &'a Item,
    key: &'static str,
) -> Result<&'a str, ManifestError> {
    match item {
        Item::Value(Value::String(s)) => Ok(s.value().as_str()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "string",
        }),
    }
}

pub(crate) fn expect_bool(item: &Item, key: &'static str) -> Result<bool, ManifestError> {
    match item {
        Item::Value(Value::Boolean(b)) => Ok(*b.value()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "boolean",
        }),
    }
}

pub(crate) fn expect_integer(item: &Item, key: &'static str) -> Result<i64, ManifestError> {
    match item {
        Item::Value(Value::Integer(i)) => Ok(*i.value()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "integer",
        }),
    }
}

pub(crate) fn expect_table<'a>(
    item: &'a Item,
    key: &'static str,
) -> Result<&'a dyn toml_edit::TableLike, ManifestError> {
    item.as_table_like()
        .ok_or_else(|| ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "table",
        })
}

fn expect_string_ctx<'a>(
    item: &'a Item,
    context: &'static str,
    field: &'static str,
) -> Result<&'a str, ManifestError> {
    match item {
        Item::Value(Value::String(s)) => Ok(s.value().as_str()),
        _ => Err(ManifestError::TypeMismatch {
            key: format!("{context}.{field}"),
            expected: "string",
        }),
    }
}

fn static_field(context: &'static str, field: &'static str) -> &'static str {
    match (context, field) {
        ("gradle", "group") => "gradle.group",
        ("gradle", "artifact") => "gradle.artifact",
        ("gradle", "version") => "gradle.version",
        ("plugin.gradle", "group") => "plugin.gradle.group",
        ("plugin.gradle", "artifact") => "plugin.gradle.artifact",
        ("plugin.gradle", "version") => "plugin.gradle.version",
        ("swift_package", "url") => "swift_package.url",
        ("swift_package", "product") => "swift_package.product",
        ("swift_package", "from_version") => "swift_package.from_version",
        ("plugin.swift_package", "url") => "plugin.swift_package.url",
        ("plugin.swift_package", "product") => "plugin.swift_package.product",
        ("plugin.swift_package", "from_version") => "plugin.swift_package.from_version",
        _ => "unknown",
    }
}
