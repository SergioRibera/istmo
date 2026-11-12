//! Build-script entry points: [`emit`] (plugin and app crates alike),
//! the plugin-side `DEP_*` handover and the auto-wiring env vars read by
//! `istmo::runtime!`.

use std::fs;
use std::path::Path;

use crate::assets::{AssetsPayload, resolve_assets};
use crate::handover::{emit_assets, emit_contract, emit_manifest, emit_native_deps};
use crate::manifest::{Deployment, IstmoToml, Manifest};

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
        emit_contracts(&manifest, &root.join("src/lib.rs"));
    }
    let dep_manifests = crate::handover::collect_dep_manifests();
    let _ = emit_wiring_for(&dep_manifests, toml.manifest.as_ref());
    crate::min_versions::enforce_min_versions(&dep_manifests, toml.manifest.as_ref(), &root);
    crate::emit_app::emit_app_from(&app, &root, &toml);
}

pub fn emit_from(manifest_path: impl AsRef<Path>, source_path: impl AsRef<Path>) {
    let manifest = emit_manifest_metadata(manifest_path);
    emit_contracts(&manifest, source_path.as_ref());
}

/// Extract and hand over the contract of every plugin entry that
/// declares a `client_type`, from the trait in `source_path`.
fn emit_contracts(manifest: &Manifest, source_path: &Path) {
    let has_client = manifest.plugins.iter().any(|p| p.client_type.is_some());
    if has_client {
        crate::directive::rerun_if_changed(source_path);
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

fn emit_wiring_for(dep_manifests: &[Manifest], app_manifest: Option<&Manifest>) -> ResolvedWiring {
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
