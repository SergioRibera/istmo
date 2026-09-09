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

use toml_edit::{ArrayOfTables, DocumentMut, Item, Table, Value};

use crate::handover::{emit_contract, emit_native_deps};
use crate::native_deps::{GradleCoord, GradleDep, GradleScope, NativeDeps, SwiftPackageDep};

/// Recognised top-level keys — anything outside this set is a hard error so
/// typos surface immediately instead of being silently dropped.
const KNOWN_KEYS: &[&str] = &["plugin", "gradle", "swift_package"];
/// Recognised keys inside a `[plugin]` / `[[plugin]]` entry.
const KNOWN_PLUGIN_KEYS: &[&str] = &["id", "client_type", "gradle", "swift_package"];

/// One plugin declared in an `istmo.toml`. Single-plugin manifests produce a
/// [`Manifest`] with `plugins.len() == 1`; multi-plugin manifests carry one
/// entry per `[[plugin]]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginEntry {
    /// Dotted plugin identifier — matches the `#[istmo::plugin]` id and the
    /// wire prefix used at runtime.
    pub id: String,
    /// Fully-qualified path of the generated `<Trait>Client` type. Reserved
    /// for a future auto-wiring pass that would let `runtime!` derive its
    /// `plugins: [...]` list from `DEP_*_PLUGIN_CLIENT_TYPES`. `None` when
    /// the manifest author leaves it out; safe to omit until the auto-wiring
    /// lands.
    pub client_type: Option<String>,
}

/// Parsed representation of an `istmo.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// Every plugin the containing crate exposes, in declaration order.
    /// Always non-empty — the parser errors when no `[plugin]` / `[[plugin]]`
    /// entry is present.
    pub plugins: Vec<PluginEntry>,
    /// Native dependencies aggregated across every top-level and per-plugin
    /// `[[gradle]]` / `[[swift_package]]` entry.
    pub native_deps: NativeDeps,
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
        Ok(Self { plugins, native_deps })
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
    let item = doc.get("plugin").ok_or(ManifestError::Missing { key: "plugin" })?;
    if let Some(table) = item.as_table() {
        // Single-plugin form.
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
    Ok(PluginEntry { id, client_type })
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
/// Forwards through the standard `cargo::metadata::…` channel. Plugin
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
    let ids: Vec<&str> = manifest.plugin_ids().collect();
    println!("cargo::metadata::PLUGIN_IDS={}", ids.join(","));
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
