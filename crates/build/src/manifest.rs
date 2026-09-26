use std::fs;
use std::path::{Path, PathBuf};

use bincode::{Decode, Encode};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::handover::{emit_contract, emit_manifest, emit_native_deps};
use crate::native_deps::{GradleCoord, GradleDep, GradleScope, NativeDeps, SwiftPackageDep};

const KNOWN_KEYS: &[&str] = &[
    "plugin",
    "gradle",
    "swift_package",
    "remote_override",
    "windows_manifest_fragment",
    "app",
];

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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
pub enum Deployment {
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

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct PluginEntry {
    pub id: String,

    pub client_type: Option<String>,

    pub default_deployment: Deployment,

    /// Whether `emit_app` should auto-register this plugin's dispatcher
    /// in the generated `IstmoPluginRegistry`. Default `true`. Plugins
    /// with bespoke construction (custom config, non-standard
    /// BackendImpl signature) set this to `false` and expect the app
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
}

/// Scalar value of an [`InfoPlistEntry`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub enum InfoPlistValue {
    String(String),
    Bool(bool),
}

/// One top-level `<key>` / value pair contributed to an iOS app's
/// `Info.plist`, declared under `[plugin.info_plist]` (plugin crates)
/// or `[app.info_plist]` (apps, where it overrides plugin defaults).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Encode, Decode)]
pub struct InfoPlistEntry {
    pub key: String,
    pub value: InfoPlistValue,
}

impl InfoPlistEntry {
    /// Render the entry as the `<key>…</key>` + value element pair that
    /// goes inside the root `<dict>` of an `Info.plist`.
    #[must_use]
    pub fn render(&self) -> String {
        let value = match &self.value {
            InfoPlistValue::String(s) => format!("<string>{}</string>", xml_escape(s)),
            InfoPlistValue::Bool(true) => "<true/>".to_owned(),
            InfoPlistValue::Bool(false) => "<false/>".to_owned(),
        };
        format!("<key>{}</key>\n{value}\n", xml_escape(&self.key))
    }

    /// Parse every key of an `info_plist` TOML table. `context` is the
    /// dotted path used in error messages (`plugin.info_plist`,
    /// `app.info_plist`).
    pub fn parse_table(
        table: &dyn toml_edit::TableLike,
        context: &str,
    ) -> Result<Vec<Self>, ManifestError> {
        table
            .iter()
            .map(|(key, item)| {
                let value = match item {
                    Item::Value(Value::String(s)) => InfoPlistValue::String(s.value().clone()),
                    Item::Value(Value::Boolean(b)) => InfoPlistValue::Bool(*b.value()),
                    _ => {
                        return Err(ManifestError::TypeMismatch {
                            key: format!("{context}.{key}"),
                            expected: "string or boolean",
                        });
                    }
                };
                Ok(Self {
                    key: key.to_owned(),
                    value,
                })
            })
            .collect()
    }
}

fn xml_escape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            other => out.push(other),
        }
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct AndroidServiceSpec {
    pub class_name: String,
    pub foreground_service_type: Option<String>,
    pub exported: bool,
    pub permission: Option<String>,
    pub process: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct IosBackgroundSpec {
    pub class_name: String,
    pub task_identifier: Option<String>,
    pub kind: IosBackgroundKindSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum IosContinuousModeSpec {
    Audio,
    Location,
    Voip,
    ExternalAccessory,
    BluetoothCentral,
    BluetoothPeripheral,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
pub struct WindowsManifestFragment {
    pub name: String,
    pub xml: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Manifest {
    pub plugins: Vec<PluginEntry>,

    pub native_deps: NativeDeps,

    pub remote_overrides: Vec<RemoteOverride>,

    pub windows_manifest_fragments: Vec<WindowsManifestFragment>,
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

    fn from_document(doc: &DocumentMut) -> Result<Self, ManifestError> {
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
        Ok(Self {
            plugins,
            native_deps,
            remote_overrides,
            windows_manifest_fragments,
        })
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
        let mut entries = Vec::with_capacity(array.len());
        for table in array {
            entries.push(parse_plugin_entry(table, "plugin", native_deps)?);
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
    let client_type = match table.get("client_type") {
        Some(item) => Some(expect_string(item, "plugin.client_type")?.to_owned()),
        None => None,
    };
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
    Ok(PluginEntry {
        id,
        client_type,
        default_deployment,
        auto_register,
        android_service,
        ios_background,
        info_plist,
    })
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
                from_version = Some(expect_string_ctx(item, context, "from_version")?)
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

fn expect_array_of_tables<'a>(
    item: &'a Item,
    key: &'static str,
) -> Result<&'a ArrayOfTables, ManifestError> {
    item.as_array_of_tables()
        .ok_or_else(|| ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "array of tables ([[…]])",
        })
}

fn expect_string<'a>(item: &'a Item, key: &'static str) -> Result<&'a str, ManifestError> {
    match item {
        Item::Value(Value::String(s)) => Ok(s.value().as_str()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "string",
        }),
    }
}

fn expect_bool(item: &Item, key: &'static str) -> Result<bool, ManifestError> {
    match item {
        Item::Value(Value::Boolean(b)) => Ok(*b.value()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "boolean",
        }),
    }
}

fn expect_integer(item: &Item, key: &'static str) -> Result<i64, ManifestError> {
    match item {
        Item::Value(Value::Integer(i)) => Ok(*i.value()),
        _ => Err(ManifestError::TypeMismatch {
            key: key.to_owned(),
            expected: "integer",
        }),
    }
}

fn expect_table<'a>(
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

pub fn emit_manifest_metadata(path: impl AsRef<Path>) -> Manifest {
    let path = path.as_ref();
    println!("cargo:rerun-if-changed={}", path.display());
    let manifest = Manifest::from_path(path).unwrap_or_else(|err| {
        panic!("istmo-build: {err}");
    });
    if !manifest.native_deps.is_empty() {
        emit_native_deps(&manifest.native_deps);
    }

    emit_manifest(&manifest);
    let ids: Vec<&str> = manifest.plugin_ids().collect();
    println!("cargo:PLUGIN_IDS={}", ids.join(","));

    // Advertise the plugin crate's `native/<platform>/` directories so
    // consuming apps' `emit_app` can copy the reference Kotlin / Swift
    // sources into the app tree without hand-copying them. Missing
    // directories are silently skipped — mobile-only plugins ship
    // Android + iOS files, native-hosted plugins may ship all three.
    let crate_root = path.parent().unwrap_or_else(|| Path::new("."));
    let native_root = crate_root.join("native");
    for (platform, env_suffix) in [("android", "ANDROID"), ("ios", "IOS"), ("macos", "MACOS")] {
        let dir = native_root.join(platform);
        if dir.is_dir() {
            let display = fs::canonicalize(&dir).unwrap_or(dir);
            println!("cargo:rerun-if-changed={}", display.display());
            println!("cargo:ISTMO_NATIVE_{env_suffix}={}", display.display());
        }
    }

    manifest
}

pub fn emit_manifest_metadata_with_contract(
    path: impl AsRef<Path>,
    contract: &crate::Contract,
) -> Manifest {
    let manifest = emit_manifest_metadata(path);
    emit_contract(contract);
    manifest
}

pub fn emit() {
    emit_with(crate::emit_app::AppOpts::default());
}

pub fn emit_with(app: crate::emit_app::AppOpts) {
    let manifest_path = Path::new("istmo.toml");
    let manifest_exists = manifest_path.exists();

    if manifest_exists {
        emit_from(manifest_path, "src/lib.rs");
    }
    let app_toml = manifest_exists.then_some(manifest_path);
    let _ = emit_wiring_env(app_toml);
    crate::emit_app::emit_app_with(app);
}

pub fn emit_from(manifest_path: impl AsRef<Path>, source_path: impl AsRef<Path>) {
    let manifest = emit_manifest_metadata(manifest_path);
    let source_path = source_path.as_ref();

    let has_client = manifest.plugins.iter().any(|p| p.client_type.is_some());
    if has_client {
        println!("cargo:rerun-if-changed={}", source_path.display());
    }

    for plugin in &manifest.plugins {
        let Some(client_type) = plugin.client_type.as_deref() else {
            continue;
        };
        let trait_name = trait_from_client_type(client_type);
        let contract = crate::extract_contract(source_path, &trait_name).unwrap_or_else(|err| {
            panic!(
                "istmo-build emit(): extract `{trait_name}` for plugin `{}` from `{}`: {err}",
                plugin.id,
                source_path.display(),
            )
        });
        emit_contract(&contract);
    }
}

fn trait_from_client_type(client_type: &str) -> String {
    let last = client_type.rsplit("::").next().unwrap_or(client_type);
    last.strip_suffix("Client")
        .unwrap_or_else(|| {
            panic!(
                "istmo-build emit(): `client_type = \"{client_type}\"` \
                 does not end in `Client` — cannot derive trait name"
            )
        })
        .to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedWiring {
    pub local_clients: Vec<String>,

    pub remote_clients: Vec<String>,
}

#[must_use]
pub fn resolve_wiring(
    dep_manifests: &[Manifest],
    app_manifest: Option<&Manifest>,
) -> ResolvedWiring {
    let mut overrides: std::collections::HashMap<&str, Deployment> =
        std::collections::HashMap::new();
    if let Some(app) = app_manifest {
        for ov in &app.remote_overrides {
            overrides.insert(ov.plugin.as_str(), ov.deployment);
        }
    }
    let mut local = Vec::new();
    let mut remote = Vec::new();
    for manifest in dep_manifests {
        for entry in &manifest.plugins {
            let Some(client_type) = entry.client_type.as_ref() else {
                continue;
            };
            let deployment = overrides
                .get(entry.id.as_str())
                .copied()
                .unwrap_or(entry.default_deployment);
            match deployment {
                Deployment::Local => local.push(client_type.clone()),
                Deployment::Remote => remote.push(client_type.clone()),
            }
        }
    }
    ResolvedWiring {
        local_clients: local,
        remote_clients: remote,
    }
}

#[must_use]
pub fn emit_wiring_env(app_manifest_path: Option<&Path>) -> ResolvedWiring {
    let dep_manifests = crate::handover::collect_dep_manifests();
    let app_manifest = app_manifest_path.and_then(|path| {
        println!("cargo:rerun-if-changed={}", path.display());
        match Manifest::from_path(path) {
            Ok(m) => Some(m),
            Err(err) => {
                println!("cargo::warning=istmo-build: failed to read app istmo.toml: {err}");
                None
            }
        }
    });
    let resolved = resolve_wiring(&dep_manifests, app_manifest.as_ref());
    println!(
        "cargo::rustc-env=ISTMO_AUTO_PLUGINS={}",
        resolved.local_clients.join(","),
    );
    println!(
        "cargo::rustc-env=ISTMO_AUTO_REMOTE={}",
        resolved.remote_clients.join(","),
    );
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(m.plugins.is_empty());
        assert_eq!(m.native_deps.gradle_entries().count(), 1);
    }

    #[test]
    fn manifest_with_only_app_section_is_valid() {
        let m = Manifest::parse("[app]\nandroid = false\n").expect("parse");
        assert!(m.plugins.is_empty());
        assert!(m.remote_overrides.is_empty());
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
    fn manifest_native_deps_survive_bincode_round_trip() {
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
