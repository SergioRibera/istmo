use std::fs;
use std::path::{Path, PathBuf};

use bincode::{Decode, Encode};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::handover::{emit_contract, emit_manifest, emit_native_deps};
use crate::native_deps::{GradleCoord, GradleDep, GradleScope, NativeDeps, SwiftPackageDep};

const KNOWN_KEYS: &[&str] = &["plugin", "gradle", "swift_package", "remote_override", "app"];

const KNOWN_PLUGIN_KEYS: &[&str] = &[
    "id",
    "client_type",
    "default_deployment",
    "gradle",
    "swift_package",
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
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct RemoteOverride {
    pub plugin: String,
    pub deployment: Deployment,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Manifest {

    pub plugins: Vec<PluginEntry>,

    pub native_deps: NativeDeps,

    pub remote_overrides: Vec<RemoteOverride>,
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
                return Err(ManifestError::UnknownKey { key: name.to_owned() });
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
        Ok(Self { plugins, native_deps, remote_overrides })
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

    Io { path: PathBuf, error: std::io::Error },

    Parse(toml_edit::TomlError),

    Missing { key: &'static str },

    TypeMismatch { key: String, expected: &'static str },

    UnknownKey { key: String },

    UnknownGradleScope(String),

    UnknownDeployment { key: String, value: String },
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
            return Err(ManifestError::UnknownKey { key: format!("{context}.{name}") });
        }
    }
    let id_item = table.get("id").ok_or(ManifestError::Missing { key: "plugin.id" })?;
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
    Ok(PluginEntry { id, client_type, default_deployment })
}

fn parse_remote_override(table: &Table) -> Result<RemoteOverride, ManifestError> {
    for (name, _) in table {
        if !KNOWN_OVERRIDE_KEYS.contains(&name) {
            return Err(ManifestError::UnknownKey {
                key: format!("remote_override.{name}"),
            });
        }
    }
    let plugin_item = table
        .get("plugin")
        .ok_or(ManifestError::Missing { key: "remote_override.plugin" })?;
    let plugin = expect_string(plugin_item, "remote_override.plugin")?.to_owned();
    let deployment_item = table
        .get("deployment")
        .ok_or(ManifestError::Missing { key: "remote_override.deployment" })?;
    let literal = expect_string(deployment_item, "remote_override.deployment")?;
    let deployment = Deployment::parse(literal).ok_or_else(|| ManifestError::UnknownDeployment {
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
                return Err(ManifestError::UnknownKey { key: format!("{context}.{other}") });
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
            "from_version" => from_version = Some(expect_string_ctx(item, context, "from_version")?),
            other => {
                return Err(ManifestError::UnknownKey { key: format!("{context}.{other}") });
            }
        }
    }
    Ok(SwiftPackageDep {
        url: url
            .ok_or_else(|| ManifestError::Missing { key: static_field(context, "url") })?
            .to_owned(),
        product: product
            .ok_or_else(|| ManifestError::Missing { key: static_field(context, "product") })?
            .to_owned(),
        from_version: from_version
            .ok_or_else(|| ManifestError::Missing { key: static_field(context, "from_version") })?
            .to_owned(),
    })
}

fn expect_array_of_tables<'a>(
    item: &'a Item,
    key: &'static str,
) -> Result<&'a ArrayOfTables, ManifestError> {
    item.as_array_of_tables().ok_or_else(|| ManifestError::TypeMismatch {
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

    let has_client = manifest
        .plugins
        .iter()
        .any(|p| p.client_type.is_some());
    if has_client {
        println!("cargo:rerun-if-changed={}", source_path.display());
    }

    for plugin in &manifest.plugins {
        let Some(client_type) = plugin.client_type.as_deref() else {
            continue;
        };
        let trait_name = trait_from_client_type(client_type);
        let contract = crate::extract_contract(source_path, &trait_name)
            .unwrap_or_else(|err| {
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
    ResolvedWiring { local_clients: local, remote_clients: remote }
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
        assert!(gradle.iter().any(|g| g.coord.artifact == "play-services-ads"));
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
}

