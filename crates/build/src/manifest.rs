//! `istmo.toml` — declarative plugin metadata.
//!
//! Plugin crates ship an `istmo.toml` alongside their `Cargo.toml`. The file
//! carries the native-build metadata that cannot be derived from the
//! `#[istmo::plugin]` trait: Gradle coordinates the app must add on Android,
//! `SwiftPM` products it must add on iOS.
//!
//! A plugin's `build.rs` collapses to a single call:
//!
//! ```no_run
//! istmo_build::emit_manifest_metadata("istmo.toml");
//! ```
//!
//! which parses the manifest and forwards its [`NativeDeps`] slice through
//! the existing [`crate::handover`] channel. Downstream apps aggregate the
//! contributions of every plugin via [`crate::collect_dep_native_deps`],
//! producing one merged Gradle fragment.
//!
//! # Schema
//!
//! Two equivalent forms — a crate ships **one plugin** with `[plugin]`, or
//! **several plugins** with `[[plugin]]`. Both accept top-level `[[gradle]]`
//! and `[[swift_package]]` blocks; multi-plugin form additionally accepts
//! per-plugin `[[plugin.gradle]]` / `[[plugin.swift_package]]` nested blocks
//! for deps that only belong to one plugin. Every dep — top-level or nested —
//! merges into the same [`NativeDeps`] bundle because Gradle / SPM dedupe on
//! `(scope, group, artifact)` / `(url, product)` regardless of source.
//!
//! ## Single-plugin form
//!
//! ```toml
//! [plugin]
//! id = "istmo.google_sign_in"
//! # `client_type` (optional) reserved for a future auto-wiring pass —
//! # the fully-qualified path of the `<Trait>Client` this plugin generates.
//! # client_type = "::istmo_google_sign_in::SignInClient"
//!
//! [[gradle]]
//! scope = "implementation"          # optional, defaults to "implementation"
//! group = "androidx.credentials"
//! artifact = "credentials"
//! version = "1.3.0"
//!
//! [[swift_package]]
//! url = "https://github.com/google/GoogleSignIn-iOS.git"
//! product = "GoogleSignIn"
//! from_version = "7.0.0"
//! ```
//!
//! ## Multi-plugin form
//!
//! ```toml
//! [[plugin]]
//! id = "istmo.google_sign_in"
//!
//!   [[plugin.gradle]]
//!   group = "androidx.credentials"
//!   artifact = "credentials"
//!   version = "1.3.0"
//!
//! [[plugin]]
//! id = "istmo.admob"
//!
//!   [[plugin.gradle]]
//!   group = "com.google.android.gms"
//!   artifact = "play-services-ads"
//!   version = "23.0.0"
//!
//! # Optional: shared across every plugin in this crate.
//! [[gradle]]
//! group = "androidx.core"
//! artifact = "core-ktx"
//! version = "1.13.0"
//! ```
//!
//! Unknown top-level keys or unrecognised entries surface as
//! [`ManifestError::UnknownKey`] so drift is loud.

use std::fs;
use std::path::{Path, PathBuf};

use bincode::{Decode, Encode};
use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::handover::{emit_contract, emit_manifest, emit_native_deps};
use crate::native_deps::{GradleCoord, GradleDep, GradleScope, NativeDeps, SwiftPackageDep};

/// Recognised top-level keys — anything outside this set is a hard error so
/// typos surface immediately instead of being silently dropped.
const KNOWN_KEYS: &[&str] = &["plugin", "gradle", "swift_package", "remote_override"];
/// Recognised keys inside a `[plugin]` / `[[plugin]]` entry.
const KNOWN_PLUGIN_KEYS: &[&str] = &[
    "id",
    "client_type",
    "default_deployment",
    "gradle",
    "swift_package",
];
/// Recognised keys inside a `[[remote_override]]` entry.
const KNOWN_OVERRIDE_KEYS: &[&str] = &["plugin", "deployment"];

/// Where the plugin is expected to run relative to the app process.
///
/// Set by the plugin author as [`PluginEntry::default_deployment`];
/// consuming apps override per plugin via a `[[remote_override]]` entry in
/// their own `istmo.toml`. Auto-wiring resolves the final decision by
/// applying overrides on top of the manifest defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Encode, Decode)]
pub enum Deployment {
    /// Runs in the app's default process. Cheap dispatch, shared heap.
    Local,
    /// Runs in a `:remote` process. Isolated crashes / memory / lifecycle;
    /// outbound frames route through the runtime's remote-envelope sink
    /// via [`crate::Runtime::declare_remote_plugin`] wiring.
    Remote,
}

impl Deployment {
    /// Parses the TOML literal (`"local"` / `"remote"`). Case-sensitive on
    /// purpose — matches the wire codec's stance elsewhere.
    fn parse(literal: &str) -> Option<Self> {
        match literal {
            "local" => Some(Self::Local),
            "remote" => Some(Self::Remote),
            _ => None,
        }
    }
}

/// One plugin declared in an `istmo.toml`. Single-plugin manifests produce a
/// [`Manifest`] with `plugins.len() == 1`; multi-plugin manifests carry one
/// entry per `[[plugin]]`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct PluginEntry {
    /// Dotted plugin identifier — matches the `#[istmo::plugin]` id and the
    /// wire prefix used at runtime.
    pub id: String,
    /// Fully-qualified path of the generated `<Trait>Client` type. Consumed
    /// by [`crate::emit_wiring_env`] to auto-populate the `plugins:` /
    /// `remote:` sections of `istmo::runtime!`. `None` skips the plugin in
    /// auto-wiring (only its native-dep contribution lands).
    pub client_type: Option<String>,
    /// Where the plugin author expects the plugin to run. `Local` (default)
    /// means outbound frames stay on the typed pump; `Remote` steers them
    /// into the `:remote` bridge sink. Overridable app-side.
    pub default_deployment: Deployment,
}

/// One entry in a consuming app's `[[remote_override]]` section. Flips the
/// deployment target for a specific plugin regardless of the manifest's
/// [`PluginEntry::default_deployment`] hint.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct RemoteOverride {
    pub plugin: String,
    pub deployment: Deployment,
}

/// Parsed representation of an `istmo.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Manifest {
    /// Every plugin the containing crate exposes, in declaration order.
    /// Plugin manifests always carry ≥ 1 entry; app-side manifests that
    /// only contribute `[[remote_override]]` may leave this empty (the
    /// parser is lenient in that case, requiring at least one plugin OR
    /// override).
    pub plugins: Vec<PluginEntry>,
    /// Native dependencies aggregated across every top-level and per-plugin
    /// `[[gradle]]` / `[[swift_package]]` entry.
    pub native_deps: NativeDeps,
    /// App-side deployment overrides — takes precedence over each plugin's
    /// [`PluginEntry::default_deployment`] during
    /// [`crate::emit_wiring_env`] resolution. Empty for plugin manifests
    /// (they have no consumers to override for).
    pub remote_overrides: Vec<RemoteOverride>,
}

impl Manifest {
    /// Parses a manifest from its serialized TOML form.
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let doc: DocumentMut = source.parse().map_err(ManifestError::Parse)?;
        Self::from_document(&doc)
    }

    /// Reads and parses `istmo.toml` at `path`.
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
        if plugins.is_empty() && remote_overrides.is_empty() {
            return Err(ManifestError::Missing { key: "plugin" });
        }
        Ok(Self { plugins, native_deps, remote_overrides })
    }

    /// The first plugin's id. Convenience for callers that expect a
    /// single-plugin manifest (the historical shape). Panics only if the
    /// manifest was constructed by hand with an empty `plugins` list — the
    /// parser guarantees at least one entry.
    #[must_use]
    pub fn primary_id(&self) -> &str {
        &self.plugins[0].id
    }

    /// Iterator over every plugin id declared in the manifest.
    pub fn plugin_ids(&self) -> impl Iterator<Item = &str> + '_ {
        self.plugins.iter().map(|p| p.id.as_str())
    }

    /// Consumes the manifest and returns just the native-dependency bundle.
    /// Convenience for callers that only care about the Gradle / SPM entries.
    #[must_use]
    pub fn into_native_deps(self) -> NativeDeps {
        self.native_deps
    }
}

/// Failure modes for [`Manifest::parse`] / [`Manifest::from_path`].
#[derive(Debug)]
pub enum ManifestError {
    /// The file at the given path could not be read.
    Io { path: PathBuf, error: std::io::Error },
    /// The bytes were not valid TOML.
    Parse(toml_edit::TomlError),
    /// A required key was missing.
    Missing { key: &'static str },
    /// A value had the wrong shape (e.g. string expected, integer found).
    TypeMismatch { key: String, expected: &'static str },
    /// A top-level key or a table entry was not in the recognised set.
    UnknownKey { key: String },
    /// A [`GradleScope`] literal was not one of the four supported values.
    UnknownGradleScope(String),
    /// A [`Deployment`] literal was not `"local"` or `"remote"`.
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
        // App-side manifests may ship only `[[remote_override]]`; the caller
        // enforces that at least one of plugin/override is present.
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

/// Preserved-static path composed of two known-at-compile-time components.
/// Used to keep [`ManifestError::Missing::key`] as `&'static str` while still
/// naming the context (`gradle` vs `plugin.gradle`) precisely.
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

/// Parse `istmo.toml` at `path` and emit its native-dep contribution.
///
/// Forwards through the standard `cargo:KEY=VALUE` channel. Plugin
/// `build.rs` convenience — one call replaces the hand-written
/// [`NativeDeps`] construction + [`emit_native_deps`] pair.
///
/// Every plugin id declared in the manifest is exposed to downstream build
/// scripts as `DEP_<links>_PLUGIN_IDS` (comma-separated; a single-plugin
/// manifest produces a one-element list). When the manifest contributes no
/// native dependencies the [`NativeDeps`] emission is skipped, saving one
/// Cargo hop.
///
/// A `cargo:rerun-if-changed=<path>` line is printed unconditionally so
/// builds pick up manifest edits.
///
/// # Panics
///
/// Panics on parse / IO failure. `build.rs` scripts have no recovery path,
/// and a manifest error is a plugin-author bug — surface it immediately.
pub fn emit_manifest_metadata(path: impl AsRef<Path>) -> Manifest {
    let path = path.as_ref();
    println!("cargo:rerun-if-changed={}", path.display());
    let manifest = Manifest::from_path(path).unwrap_or_else(|err| {
        panic!("istmo-build: {err}");
    });
    if !manifest.native_deps.is_empty() {
        emit_native_deps(&manifest.native_deps);
    }
    // Full manifest — bincode-encoded — carries `client_type`,
    // `default_deployment` and the plugin list needed by
    // [`emit_wiring_env`]. `PLUGIN_IDS` stays as a cheap comma-separated
    // sidecar for consumers that only want the id list without decoding.
    emit_manifest(&manifest);
    let ids: Vec<&str> = manifest.plugin_ids().collect();
    println!("cargo:PLUGIN_IDS={}", ids.join(","));
    manifest
}

/// Manifest emission bundled with a [`Contract`](crate::Contract) emission.
///
/// Handy when the plugin's `build.rs` already builds a [`Contract`] via
/// `istmo-plugins-schema`; folds both emissions behind one call.
///
/// # Panics
/// See [`emit_manifest_metadata`].
pub fn emit_manifest_metadata_with_contract(
    path: impl AsRef<Path>,
    contract: &crate::Contract,
) -> Manifest {
    let manifest = emit_manifest_metadata(path);
    emit_contract(contract);
    manifest
}

/// Resolved wiring — the shape emitted by [`emit_wiring_env`] into
/// `ISTMO_AUTO_PLUGINS` / `ISTMO_AUTO_REMOTE`. Exposed for testing;
/// production consumers only care about the env-var side effects.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ResolvedWiring {
    /// Client type paths (e.g. `::istmo_google_sign_in::SignInClient`) that
    /// stay in the app process. Auto-injected into `runtime! { plugins: [] }`.
    pub local_clients: Vec<String>,
    /// Client type paths that route through the `:remote` bridge.
    /// Auto-injected into `runtime! { remote: [] }`.
    pub remote_clients: Vec<String>,
}

/// Resolves the final auto-wiring by combining dep manifests with an
/// optional app-side override manifest. Overrides take precedence over
/// each plugin's [`PluginEntry::default_deployment`].
///
/// Plugins whose manifest carries no `client_type` are skipped — they
/// contribute only native deps and are not part of the runtime wiring.
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

/// App-side `build.rs` helper for auto-wiring `istmo::runtime!`.
///
/// Walks `DEP_*_ISTMO_MANIFEST`, optionally applies `[[remote_override]]`
/// entries from the app's own `istmo.toml`, and emits two
/// `cargo::rustc-env` pairs the `istmo::runtime!` macro reads at
/// expansion time:
///
/// * `ISTMO_AUTO_PLUGINS` — comma-separated fully-qualified `<T>Client`
///   paths that auto-populate the `plugins:` section.
/// * `ISTMO_AUTO_REMOTE` — same shape, for the `remote:` section.
///
/// Pass `Some(path)` to point at the app's own `istmo.toml`; pass
/// `None` when the app has no manifest of its own (defaults apply
/// unchanged). Missing files at the given path produce a `cargo::warning`
/// and treat the app as override-less rather than failing the build,
/// which keeps the helper safe to call unconditionally.
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
        // scope defaulted to Implementation on the second entry.
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
        // Nested-per-plugin + shared top-level all merged into one bundle.
        let gradle: Vec<_> = m.native_deps.gradle_entries().collect();
        assert_eq!(gradle.len(), 3);
        assert!(gradle.iter().any(|g| g.coord.artifact == "credentials"));
        assert!(gradle.iter().any(|g| g.coord.artifact == "play-services-ads"));
        assert!(gradle.iter().any(|g| g.coord.artifact == "core-ktx"));
        assert_eq!(m.native_deps.swift_entries().count(), 1);
    }

    #[test]
    fn missing_plugin_section_is_reported() {
        let err = Manifest::parse("[[gradle]]\ngroup=\"g\"\nartifact=\"a\"\nversion=\"1\"\n")
            .expect_err("must fail");
        assert!(matches!(err, ManifestError::Missing { key: "plugin" }));
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
        // Same channel a real cross-plugin handover would take: manifest
        // → NativeDeps → serialize → deserialize → merge.
        let m = Manifest::parse(FULL_MANIFEST).expect("parse");
        let hex = crate::serialize_native_deps(&m.native_deps).expect("serialize");
        let back = crate::deserialize_native_deps(&hex).expect("deserialize");
        assert_eq!(back, m.native_deps);
    }
}
