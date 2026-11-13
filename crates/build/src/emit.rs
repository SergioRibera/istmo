//! Build-script entry points: [`emit`] (plugin and app crates alike),
//! the plugin-side `DEP_*` handover and the auto-wiring env vars read by
//! `istmo::runtime!`.

use std::fs;
use std::path::Path;

use crate::assets::{AssetsPayload, resolve_assets};
use crate::handover::{
    contracts_handover_line, emit_assets, emit_contracts, emit_manifest, emit_native_deps,
};
use crate::manifest::{Deployment, IstmoToml, Manifest};

/// Plugin-side handover for the `istmo.toml` at `path`.
///
/// Prints native dependencies, the parsed manifest, `PLUGIN_IDS`,
/// declared assets and the crate's `native/{android,ios,macos}/`
/// directories as `cargo:` metadata for dependent build scripts.
/// Contracts are not included; see [`emit_from`].
///
/// # Panics
///
/// When the manifest cannot be read or parsed, or a declared asset does
/// not resolve — panicking is how a build script fails the build.
pub fn emit_manifest_metadata(path: impl AsRef<Path>) -> Manifest {
    let path = path.as_ref();
    let manifest = Manifest::from_path(path)
        .unwrap_or_else(|err| panic!("istmo-build: {}: {err}", path.display()));
    emit_plugin_metadata(path, manifest)
}

/// Plugin-side handover for an already parsed `istmo.toml` at `path`.
fn emit_plugin_metadata(path: &Path, manifest: Manifest) -> Manifest {
    crate::directive::rerun_if_changed(path);
    if !manifest.native_deps.is_empty() {
        emit_native_deps(&manifest.native_deps);
    }

    emit_manifest(&manifest);
    let ids: Vec<&str> = manifest.plugin_ids().collect();
    println!("cargo:PLUGIN_IDS={}", ids.join(","));

    if !manifest.assets.is_empty() {
        let crate_root = path.parent().unwrap_or_else(|| Path::new("."));
        let resolved = resolve_assets(&manifest.assets, crate_root).unwrap_or_else(|err| {
            panic!("istmo-build: {err}");
        });
        for asset in &resolved {
            crate::directive::rerun_if_changed(asset.source());
        }
        for dir in crate::assets::declared_asset_dirs(&manifest.assets, crate_root) {
            crate::directive::rerun_if_changed(&dir);
        }
        let payload = AssetsPayload {
            plugin_ids: manifest.plugin_ids().map(str::to_owned).collect(),
            resolved,
        };
        emit_assets(&payload);
    }

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
            crate::directive::rerun_if_changed(&display);
            println!("cargo:ISTMO_NATIVE_{env_suffix}={}", display.display());
        }
    }

    manifest
}

/// [`emit_manifest_metadata`] plus a hand-built contract list, for a
/// plugin crate whose contracts are not extracted from its source.
/// Pass every contract of the crate in one call.
///
/// # Panics
///
/// When `istmo.toml` is malformed or the contracts exceed the handover
/// codec limit — panicking is how a build script fails the build.
pub fn emit_manifest_metadata_with_contracts(
    path: impl AsRef<Path>,
    contracts: &[crate::Contract],
) -> Manifest {
    let manifest = emit_manifest_metadata(path);
    emit_contracts(contracts);
    manifest
}

/// The one-line build script for plugin and app crates alike:
/// [`emit_with`] with [`AppOpts::default`](crate::AppOpts).
///
/// When `istmo.toml` holds `[plugin]` entries it hands their metadata
/// and the contracts extracted from `src/lib.rs` over to dependents; it
/// then checks every dependency's `[min_versions]` and runs the app
/// side (auto-wiring env for `istmo::runtime!`, Kotlin / Swift codegen
/// for the `android/` and `ios/` projects that exist, desktop icon and
/// assets).
///
/// # Panics
///
/// As [`emit_with`].
pub fn emit() {
    emit_with(crate::emit_app::AppOpts::default());
}

/// [`emit`] with explicit options. Every path, `istmo.toml` included,
/// resolves against `app.root` (default: `CARGO_MANIFEST_DIR`), and
/// the manifest is read and parsed once for every step.
///
/// # Panics
///
/// When `istmo.toml` is malformed, a plugin contract cannot be
/// extracted, or a minimum-version check fails — panicking is how a
/// build script fails the build.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public signature kept for existing build scripts"
)]
pub fn emit_with(app: crate::emit_app::AppOpts) {
    let root = app
        .root
        .clone()
        .unwrap_or_else(crate::emit_app::cargo_manifest_dir);
    let manifest_path = root.join("istmo.toml");
    let toml = IstmoToml::load(&manifest_path);

    if let Some(manifest) = toml.manifest.clone() {
        let manifest = emit_plugin_metadata(&manifest_path, manifest);
        emit_plugin_contracts(&manifest, &root.join("src/lib.rs"));
    }
    let dep_manifests = crate::handover::collect_dep_manifests();
    crate::min_versions::enforce_min_versions(&dep_manifests, toml.manifest.as_ref(), &root);
    // Also emits the auto-wiring env, so `runtime!` and the app-side
    // surfaces read one resolved set.
    crate::emit_app::emit_app_from(&app, &root, &toml);
}

/// Plugin-side handover only; runs no app-side step.
///
/// [`emit_manifest_metadata`] for `manifest_path`, plus the contract of
/// every `[plugin]` entry with a `client_type`, extracted from the trait
/// in `source_path`.
///
/// # Panics
///
/// When the manifest is malformed or a contract cannot be extracted.
pub fn emit_from(manifest_path: impl AsRef<Path>, source_path: impl AsRef<Path>) {
    let manifest = emit_manifest_metadata(manifest_path);
    emit_plugin_contracts(&manifest, source_path.as_ref());
}

/// Extract the contract of every plugin entry that declares a
/// `client_type` from the trait in `source_path`, and hand them over in
/// one payload.
fn emit_plugin_contracts(manifest: &Manifest, source_path: &Path) {
    if let Some(line) = plugin_contracts_line(manifest, source_path) {
        crate::directive::rerun_if_changed(source_path);
        println!("{line}");
    }
}

/// The handover line [`emit_plugin_contracts`] prints, `None` when no
/// plugin entry declares a `client_type`.
fn plugin_contracts_line(manifest: &Manifest, source_path: &Path) -> Option<String> {
    let contracts = plugin_contracts(manifest, source_path);
    if contracts.is_empty() {
        return None;
    }
    let line = contracts_handover_line(&contracts)
        .unwrap_or_else(|err| panic!("istmo-build emit(): {err}"));
    Some(line)
}

/// Contracts of the plugin entries with a `client_type`, in manifest
/// order.
fn plugin_contracts(manifest: &Manifest, source_path: &Path) -> Vec<crate::Contract> {
    manifest
        .plugins
        .iter()
        .filter_map(|plugin| {
            let client_type = plugin.client_type.as_deref()?;
            let trait_name = trait_from_client_type(client_type);
            let contract =
                crate::extract_contract(source_path, &trait_name).unwrap_or_else(|err| {
                    panic!(
                        "istmo-build emit(): extract `{trait_name}` for plugin `{}` from `{}`: {err}",
                        plugin.id,
                        source_path.display(),
                    )
                });
            Some(contract)
        })
        .collect()
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

/// Resolve the auto-wired plugin set and bake it into
/// `ISTMO_AUTO_PLUGINS` / `ISTMO_AUTO_REMOTE` for `istmo::runtime!`.
///
/// # Panics
///
/// When `app_manifest_path` names an `istmo.toml` that cannot be read
/// or parsed.
#[must_use]
pub fn emit_wiring_env(app_manifest_path: Option<&Path>) -> ResolvedWiring {
    let app_manifest = app_manifest_path.map(|path| {
        crate::directive::rerun_if_changed(path);
        Manifest::from_path(path)
            .unwrap_or_else(|err| panic!("istmo-build: {}: {err}", path.display()))
    });
    emit_wiring_for(
        &crate::handover::collect_dep_manifests(),
        app_manifest.as_ref(),
    )
}

/// Bake [`resolve_wiring`] into the `cargo::rustc-env` lines `runtime!`
/// reads and return it: `rustc-env` reaches the compiler only, never
/// the build script's own environment, so the build script must keep
/// using this value rather than reading the variables back.
pub(crate) fn emit_wiring_for(
    dep_manifests: &[Manifest],
    app_manifest: Option<&Manifest>,
) -> ResolvedWiring {
    let resolved = resolve_wiring(dep_manifests, app_manifest);
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
    use crate::handover::collect_contracts_from;

    #[test]
    fn a_crate_with_two_plugins_hands_over_both_contracts() {
        let root = std::env::temp_dir().join(format!("istmo-emit-two-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let source = root.join("lib.rs");
        fs::write(
            &source,
            r#"
            #[istmo::plugin(name = "example.first")]
            pub trait First {
                async fn ping(&self) -> String;
            }

            #[istmo::plugin(name = "example.second")]
            pub trait Second {
                async fn pong(&self, n: u32) -> u32;
            }
            "#,
        )
        .unwrap();
        let manifest = Manifest::parse(
            "[[plugin]]\nid = \"example.first\"\nclient_type = \"::example::FirstClient\"\n\n\
             [[plugin]]\nid = \"example.second\"\nclient_type = \"::example::SecondClient\"\n\n\
             [[plugin]]\nid = \"example.native_only\"\n",
        )
        .unwrap();

        let line = plugin_contracts_line(&manifest, &source).expect("two plugins have contracts");
        let _ = fs::remove_dir_all(&root);

        // Cargo keeps one value per key: both contracts travel in one line.
        assert_eq!(line.lines().count(), 1);
        let (key, payload) = line
            .strip_prefix("cargo:")
            .and_then(|rest| rest.split_once('='))
            .expect("a `cargo:KEY=VALUE` line");
        assert_eq!(key, "ISTMO_CONTRACTS");
        assert!(payload.bytes().all(|b| b.is_ascii_hexdigit()));

        // What the consuming build script sees as `DEP_<LINKS>_<KEY>`.
        let got = collect_contracts_from([(format!("DEP_EXAMPLE_{key}"), payload.to_owned())]);
        let ids: Vec<(&str, &str)> = got
            .iter()
            .map(|c| (c.plugin_id.as_str(), c.type_name.as_str()))
            .collect();
        assert_eq!(
            ids,
            [("example.first", "First"), ("example.second", "Second")]
        );
        assert_eq!(got[1].methods[0].name, "pong");
    }

    #[test]
    fn a_crate_without_client_types_prints_no_contract_line() {
        let manifest = Manifest::parse("[plugin]\nid = \"example.native_only\"\n").unwrap();
        let source = std::env::temp_dir().join("istmo-emit-no-client-type-unread.rs");
        assert_eq!(plugin_contracts_line(&manifest, &source), None);
    }
}
