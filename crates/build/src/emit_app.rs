//! One-liner app-side codegen: `istmo_build::emit_app()` walks every
//! upstream plugin's `Contract` (via [`collect_dep_contracts`]) and
//! writes matching Kotlin / Swift dispatcher, types and codecs files
//! into conventional paths inside `android/` and `ios/`.
//!
//! Zero-config default: auto-detects the Android namespace from
//! `app/build.gradle{,.kts}` and the iOS app directory from a single
//! `ios/*/` subdirectory. Configuration lives in `istmo.toml` under an
//! `[app]` section — see the module docs for the schema — or is
//! passed programmatically via [`AppOpts`] to [`emit_app_with`].
//!
//! [`emit`](crate::emit) invokes [`emit_app`] automatically after the
//! plugin-side metadata pass whenever an `android/` or `ios/` sibling
//! directory exists next to `Cargo.toml`, so most apps never need to
//! call this module directly.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use toml_edit::{DocumentMut, Item, Table, Value};

use crate::apple_plist::{
    ENTITLEMENTS_FRAGMENT, INFO_PLIST_FRAGMENT, PLUGINS_MARK_END, PLUGINS_MARK_START,
    PlistFragments, app_keys_outside_block,
};
use crate::contract::Contract;
use crate::handover::{collect_dep_contracts, collect_dep_manifests};
use crate::ios::{BackgroundKind, ContinuousMode, IosBackgroundContract, generate_ios_background};
use crate::kotlin_client::generate_kotlin_client;
use crate::kotlin_host::{generate_kotlin_codecs_interface, generate_kotlin_host};
use crate::kotlin_types::{generate_kotlin_codecs, generate_kotlin_types};
use crate::manifest::{
    AndroidServiceSpec, InfoPlistEntry, IosBackgroundKindSpec, IosBackgroundSpec,
    IosContinuousModeSpec, Manifest, PluginEntry,
};
use crate::min_versions::OsVersion;
use crate::plugin_registry::{
    RegistryEntry, generate_kotlin_plugin_registry, generate_swift_plugin_registry,
};
use crate::service::{ServiceContract, generate_android_service};
use crate::swift::generate_swift_client;
use crate::swift_host::generate_swift_host;
use crate::swift_types::{generate_swift_codecs, generate_swift_types};

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

#[derive(Debug, Clone, Default)]
pub struct AppOpts {
    pub root: Option<PathBuf>,
    pub android: Option<bool>,
    pub android_root: Option<PathBuf>,
    pub android_package: Option<String>,
    pub ios: Option<bool>,
    pub ios_root: Option<PathBuf>,
    pub ios_app_dir: Option<String>,
    pub ios_plugins_subdir: Option<String>,
    pub extra_contracts: Vec<Contract>,
    /// Additional [`Manifest`] values to merge alongside those found via
    /// the `DEP_*_ISTMO_MANIFEST` env-var handover. Useful for tests
    /// and for apps that ship plugin scaffolding inline instead of
    /// through a dedicated plugin crate.
    pub extra_manifests: Vec<Manifest>,
    /// Additional `(name, native/ios dir)` pairs merged with those found
    /// via the `DEP_*_ISTMO_NATIVE_IOS` handover. Same purpose as
    /// [`Self::extra_manifests`].
    pub extra_native_ios_dirs: Vec<(String, PathBuf)>,
    pub per_plugin: HashMap<String, AppPluginOpts>,
    pub seed_backends: bool,
    /// Emit `IstmoPluginRegistry.kt` / `.swift` files that construct
    /// every plugin's dispatcher and register it with the runtime in
    /// one call. Default `true`. When `false` `emit_app` skips the
    /// registry entirely and the app author registers each dispatcher
    /// by hand.
    pub auto_register: Option<bool>,
    /// Name passed to `System.loadLibrary(...)` in generated Android
    /// service shims. Defaults to `CARGO_PKG_NAME` with hyphens folded
    /// to underscores — matches the Rust cdylib naming convention.
    pub lib_name: Option<String>,
}

pub fn emit_app() {
    emit_app_with(AppOpts::default());
}

pub fn emit_app_with(opts: AppOpts) {
    let root = opts.root.clone().unwrap_or_else(cargo_manifest_dir);

    let android_root = opts
        .android_root
        .clone()
        .unwrap_or_else(|| root.join("android"));
    let ios_root = opts.ios_root.clone().unwrap_or_else(|| root.join("ios"));

    let manifest_path = root.join("istmo.toml");
    let (app_config, manifest_source) = if manifest_path.exists() {
        match fs::read_to_string(&manifest_path) {
            Ok(text) => match text.parse::<DocumentMut>() {
                Ok(doc) => (parse_app_config(&doc), Some(text)),
                Err(err) => {
                    println!(
                        "cargo::warning=istmo-build: failed to parse {}: {err}",
                        manifest_path.display()
                    );
                    (AppConfig::default(), None)
                }
            },
            Err(err) => {
                println!(
                    "cargo::warning=istmo-build: failed to read {}: {err}",
                    manifest_path.display()
                );
                (AppConfig::default(), None)
            }
        }
    } else {
        (AppConfig::default(), None)
    };
    let _ = manifest_source;

    let android_enabled = opts.android.or(app_config.android).unwrap_or(true);
    let ios_enabled = opts.ios.or(app_config.ios).unwrap_or(true);

    let android_active = android_enabled && android_root.is_dir();
    let ios_active = ios_enabled && ios_root.is_dir();
    if !android_active && !ios_active {
        return;
    }

    let android_package = opts
        .android_package
        .clone()
        .or_else(|| app_config.android_package.clone())
        .or_else(|| detect_android_package(&android_root).map(|ns| format!("{ns}.gen")));
    let ios_app_dir = opts
        .ios_app_dir
        .clone()
        .or_else(|| app_config.ios_app_dir.clone())
        .or_else(|| detect_ios_app_dir(&ios_root));
    let ios_plugins_subdir = opts
        .ios_plugins_subdir
        .clone()
        .or_else(|| app_config.ios_plugins_subdir.clone())
        .unwrap_or_else(|| "Plugins".to_owned());

    let mut per_plugin = app_config.per_plugin;
    for (k, v) in opts.per_plugin.clone() {
        per_plugin.insert(k, v);
    }

    let mut contracts = collect_dep_contracts();
    contracts.extend(opts.extra_contracts.iter().cloned());

    if contracts.is_empty() {
        return;
    }

    let mut dep_manifests = collect_dep_manifests();
    dep_manifests.extend(opts.extra_manifests.iter().cloned());
    let plugin_auto_register: HashMap<String, bool> = dep_manifests
        .iter()
        .flat_map(|m| m.plugins.iter())
        .map(|p| (p.id.clone(), p.auto_register))
        .collect();

    let app_auto_register = opts
        .auto_register
        .or(app_config.auto_register)
        .unwrap_or(true);

    let mut kotlin_registry: Vec<RegistryEntry> = Vec::new();
    let mut swift_registry: Vec<RegistryEntry> = Vec::new();

    for contract in &contracts {
        let overrides = per_plugin
            .get(contract.plugin_id.as_str())
            .or_else(|| per_plugin.get(contract.type_name.as_str()));
        let role = overrides.and_then(|o| o.role).unwrap_or(Role::Host);
        let platforms = overrides
            .and_then(|o| o.platforms.clone())
            .unwrap_or_else(|| {
                let mut ps = Vec::new();
                if android_active {
                    ps.push(Platform::Android);
                }
                if ios_active {
                    ps.push(Platform::Ios);
                }
                ps
            });

        let plugin_declared_auto = plugin_auto_register
            .get(contract.plugin_id.as_str())
            .copied()
            .unwrap_or(true);
        let app_override = overrides.and_then(|o| o.auto_register);
        let auto_register_this = app_auto_register
            && plugin_declared_auto
            && app_override.unwrap_or(true)
            && role == Role::Host;

        for platform in &platforms {
            match platform {
                Platform::Android => {
                    if !android_active {
                        continue;
                    }
                    let Some(pkg) = android_package.as_deref() else {
                        println!(
                            "cargo::warning=istmo-build: cannot resolve Android namespace for `{}`; set `[app] android_package` in istmo.toml",
                            contract.type_name
                        );
                        continue;
                    };
                    emit_kotlin(&android_root, pkg, contract, role, opts.seed_backends);
                    if auto_register_this {
                        if let Some(entry) = RegistryEntry::from_contract(contract) {
                            kotlin_registry.push(entry);
                        }
                    }
                }
                Platform::Ios => {
                    if !ios_active {
                        continue;
                    }
                    let Some(app_dir) = ios_app_dir.as_deref() else {
                        println!(
                            "cargo::warning=istmo-build: cannot resolve iOS app dir for `{}`; set `[app] ios_app_dir` in istmo.toml",
                            contract.type_name
                        );
                        continue;
                    };
                    emit_swift(
                        &ios_root,
                        app_dir,
                        &ios_plugins_subdir,
                        contract,
                        role,
                        opts.seed_backends,
                    );
                    if auto_register_this {
                        if let Some(entry) = RegistryEntry::from_contract(contract) {
                            swift_registry.push(entry);
                        }
                    }
                }
            }
        }
    }

    if android_active && app_auto_register {
        if let Some(pkg) = android_package.as_deref() {
            emit_kotlin_registry(&android_root, pkg, &kotlin_registry);
        }
    }
    if ios_active && app_auto_register {
        if let Some(app_dir) = ios_app_dir.as_deref() {
            emit_swift_registry(&ios_root, app_dir, &ios_plugins_subdir, &swift_registry);
        }
    }
    if ios_active {
        if let Some(app_dir) = ios_app_dir.as_deref() {
            let mut native_dirs = dep_native_ios_dirs();
            native_dirs.extend(
                opts.extra_native_ios_dirs
                    .iter()
                    .map(|(name, dir)| (name.clone(), dir.display().to_string())),
            );
            let ios_min = dep_manifests
                .iter()
                .filter_map(|m| m.min_versions.ios.map(|v| (v, m.primary_id().to_owned())))
                .max_by_key(|(v, _)| *v);
            emit_ios_plugins_fragment(&ios_root, app_dir, &native_dirs, ios_min.as_ref());
            emit_ios_plist_fragments(&ios_root, app_dir, &native_dirs);
        }
    }

    let lib_name = opts
        .lib_name
        .clone()
        .or_else(default_lib_name)
        .unwrap_or_else(|| "istmo_app".to_owned());

    emit_service_and_background(
        &dep_manifests,
        ServiceEmitOpts {
            android_active,
            android_root: &android_root,
            android_package: android_package.as_deref(),
            ios_active,
            ios_root: &ios_root,
            ios_app_dir: ios_app_dir.as_deref(),
            ios_plugins_subdir: &ios_plugins_subdir,
            ios_info_plist_overrides: &app_config.info_plist,
            lib_name: &lib_name,
        },
    );

    println!("cargo:rerun-if-changed=build.rs");
    if manifest_path.exists() {
        println!("cargo:rerun-if-changed=istmo.toml");
    }
}

struct ServiceEmitOpts<'a> {
    android_active: bool,
    android_root: &'a Path,
    android_package: Option<&'a str>,
    ios_active: bool,
    ios_root: &'a Path,
    ios_app_dir: Option<&'a str>,
    ios_plugins_subdir: &'a str,
    ios_info_plist_overrides: &'a [InfoPlistEntry],
    lib_name: &'a str,
}

fn default_lib_name() -> Option<String> {
    std::env::var("CARGO_PKG_NAME")
        .ok()
        .map(|n| n.replace('-', "_"))
}

fn emit_service_and_background(dep_manifests: &[Manifest], opts: ServiceEmitOpts<'_>) {
    let mut android_manifest_fragments: Vec<String> = Vec::new();
    let mut ios_plist_fragments: Vec<String> = Vec::new();

    for manifest in dep_manifests {
        for plugin in &manifest.plugins {
            if let (true, Some(spec)) = (opts.android_active, plugin.android_service.as_ref()) {
                match opts.android_package {
                    Some(pkg) => {
                        let fragment = emit_android_service(
                            opts.android_root,
                            pkg,
                            opts.lib_name,
                            plugin,
                            spec,
                        );
                        android_manifest_fragments.push(fragment);
                    }
                    None => {
                        println!(
                            "cargo::warning=istmo-build: android_service on `{}` skipped — \
                             android package unresolved (set [app] android_package)",
                            plugin.id
                        );
                    }
                }
            }
            if let (true, Some(spec)) = (opts.ios_active, plugin.ios_background.as_ref()) {
                match opts.ios_app_dir {
                    Some(app_dir) => {
                        let fragment = emit_ios_background(
                            opts.ios_root,
                            app_dir,
                            opts.ios_plugins_subdir,
                            plugin,
                            spec,
                        );
                        ios_plist_fragments.push(fragment);
                    }
                    None => {
                        println!(
                            "cargo::warning=istmo-build: ios_background on `{}` skipped — \
                             ios app dir unresolved (set [app] ios_app_dir)",
                            plugin.id
                        );
                    }
                }
            }
        }
    }

    if opts.ios_active {
        let entries = merge_info_plist_entries(dep_manifests, opts.ios_info_plist_overrides);
        if !entries.is_empty() {
            let mut fragment = String::from("<!-- GENERATED by istmo-build - DO NOT EDIT. -->\n");
            for entry in &entries {
                fragment.push_str(&entry.render());
            }
            ios_plist_fragments.push(fragment);
        }
    }

    if opts.android_active
        && !android_manifest_fragments.is_empty()
        && opts.android_package.is_some()
    {
        write_android_manifest_sidecar(opts.android_root, &android_manifest_fragments);
        patch_android_manifest(opts.android_root, &android_manifest_fragments);
    }
    if opts.ios_active && !ios_plist_fragments.is_empty() {
        if let Some(app_dir) = opts.ios_app_dir {
            write_ios_plist_sidecar(opts.ios_root, app_dir, &ios_plist_fragments);
            patch_ios_info_plist(opts.ios_root, app_dir, &ios_plist_fragments);
        }
    }
}

/// Collect every plugin's `[plugin.info_plist]` entries in declaration
/// order, keeping the first value when two plugins disagree on a key,
/// then apply the app's `[app.info_plist]` overrides on top.
fn merge_info_plist_entries(
    dep_manifests: &[Manifest],
    app_overrides: &[InfoPlistEntry],
) -> Vec<InfoPlistEntry> {
    let mut merged: Vec<InfoPlistEntry> = Vec::new();
    for plugin in dep_manifests.iter().flat_map(|m| m.plugins.iter()) {
        for entry in &plugin.info_plist {
            match merged.iter().find(|e| e.key == entry.key) {
                Some(existing) if existing.value != entry.value => println!(
                    "cargo::warning=istmo-build: Info.plist key `{}` declared with different \
                     values by several plugins (`{}` ignored) — set it in `[app.info_plist]`",
                    entry.key, plugin.id,
                ),
                Some(_) => {}
                None => merged.push(entry.clone()),
            }
        }
    }
    for entry in app_overrides {
        match merged.iter_mut().find(|e| e.key == entry.key) {
            Some(existing) => existing.value = entry.value.clone(),
            None => merged.push(entry.clone()),
        }
    }
    merged
}

fn emit_android_service(
    android_root: &Path,
    package: &str,
    lib_name: &str,
    plugin: &PluginEntry,
    spec: &AndroidServiceSpec,
) -> String {
    let contract = ServiceContract {
        plugin_id: plugin.id.clone(),
        class_name: spec.class_name.clone(),
        kotlin_package: package.to_owned(),
        lib_name: lib_name.to_owned(),
        runtime_package: "dev.istmo.runtime".to_owned(),
        foreground_service_type: spec.foreground_service_type.clone(),
        exported: spec.exported,
        permission: spec.permission.clone(),
        process: spec.process.clone(),
    };
    let artifacts = generate_android_service(&contract);
    let pkg_path = package.replace('.', "/");
    let dest = android_root
        .join("app/src/main/java")
        .join(pkg_path)
        .join(format!("{}.kt", spec.class_name));
    write_if_changed(&dest, &artifacts.kotlin);
    artifacts.manifest_fragment
}

fn emit_ios_background(
    ios_root: &Path,
    app_dir: &str,
    plugins_subdir: &str,
    plugin: &PluginEntry,
    spec: &IosBackgroundSpec,
) -> String {
    let contract = IosBackgroundContract {
        plugin_id: plugin.id.clone(),
        class_name: spec.class_name.clone(),
        task_identifier: spec.task_identifier.clone(),
        kind: ios_kind_from_spec(&spec.kind),
    };
    let artifacts = generate_ios_background(&contract);
    let dest = ios_root
        .join(app_dir)
        .join(plugins_subdir)
        .join("Background")
        .join(format!("{}.swift", spec.class_name));
    write_if_changed(&dest, &artifacts.swift);
    artifacts.info_plist_fragment
}

fn ios_kind_from_spec(spec: &IosBackgroundKindSpec) -> BackgroundKind {
    match spec {
        IosBackgroundKindSpec::Refresh { interval_minutes } => BackgroundKind::Refresh {
            interval_minutes: *interval_minutes,
        },
        IosBackgroundKindSpec::Processing {
            requires_power,
            requires_network,
        } => BackgroundKind::Processing {
            requires_power: *requires_power,
            requires_network: *requires_network,
        },
        IosBackgroundKindSpec::Continuous(mode) => BackgroundKind::Continuous(match mode {
            IosContinuousModeSpec::Audio => ContinuousMode::Audio,
            IosContinuousModeSpec::Location => ContinuousMode::Location,
            IosContinuousModeSpec::Voip => ContinuousMode::Voip,
            IosContinuousModeSpec::ExternalAccessory => ContinuousMode::ExternalAccessory,
            IosContinuousModeSpec::BluetoothCentral => ContinuousMode::BluetoothCentral,
            IosContinuousModeSpec::BluetoothPeripheral => ContinuousMode::BluetoothPeripheral,
        }),
    }
}

const ANDROID_MARK_START: &str = "<!-- istmo:services:start -->";
const ANDROID_MARK_END: &str = "<!-- istmo:services:end -->";
const IOS_MARK_START: &str = "<!-- istmo:background:start -->";
const IOS_MARK_END: &str = "<!-- istmo:background:end -->";

fn write_android_manifest_sidecar(android_root: &Path, fragments: &[String]) {
    let dest = android_root
        .join("app/src/main")
        .join("AndroidManifest.services.xml");
    let body = join_fragments(fragments);
    let (start, end) = (
        marker_label(ANDROID_MARK_START),
        marker_label(ANDROID_MARK_END),
    );
    let contents = format!(
        "<!-- GENERATED by istmo-build - DO NOT EDIT. -->\n\
         <!-- Copy the child element(s) below inside your <application> tag, -->\n\
         <!-- or add the `{start}` / `{end}` comment markers inside it -->\n\
         <!-- so emit_app can patch AndroidManifest.xml directly. -->\n\
         {body}"
    );
    write_if_changed(&dest, &contents);
}

fn write_ios_plist_sidecar(ios_root: &Path, app_dir: &str, fragments: &[String]) {
    let dest = ios_root.join(app_dir).join("Info.plist.background.xml");
    let body = join_fragments(fragments);
    let (start, end) = (marker_label(IOS_MARK_START), marker_label(IOS_MARK_END));
    let contents = format!(
        "<!-- GENERATED by istmo-build - DO NOT EDIT. -->\n\
         <!-- Copy the entries below into your Info.plist <dict>, or add the -->\n\
         <!-- `{start}` / `{end}` comment markers inside it -->\n\
         <!-- so emit_app can patch Info.plist directly. -->\n\
         {body}"
    );
    write_if_changed(&dest, &contents);
}

/// Text of a `<!-- … -->` marker without its delimiters, so it can be
/// quoted inside another comment (XML comments do not nest).
fn marker_label(marker: &str) -> &str {
    marker
        .trim_start_matches("<!--")
        .trim_end_matches("-->")
        .trim()
}

fn patch_android_manifest(android_root: &Path, fragments: &[String]) {
    let path = android_root.join("app/src/main/AndroidManifest.xml");
    patch_marker_file(&path, ANDROID_MARK_START, ANDROID_MARK_END, fragments);
}

fn patch_ios_info_plist(ios_root: &Path, app_dir: &str, fragments: &[String]) {
    let path = ios_root.join(app_dir).join("Info.plist");
    patch_marker_file(&path, IOS_MARK_START, IOS_MARK_END, fragments);
}

fn patch_marker_file(path: &Path, start: &str, end: &str, fragments: &[String]) {
    let Ok(source) = fs::read_to_string(path) else {
        return;
    };
    let Some(start_idx) = source.find(start) else {
        return;
    };
    let Some(end_idx) = source[start_idx..].find(end).map(|i| start_idx + i) else {
        println!(
            "cargo::warning=istmo-build: {} contains `{}` without matching `{}` — \
             skipping istmo-managed block replacement",
            path.display(),
            start,
            end,
        );
        return;
    };
    let mut managed = String::new();
    managed.push_str(start);
    managed.push('\n');
    managed.push_str(&join_fragments(fragments));
    managed.push_str(end);
    let mut next = String::with_capacity(source.len());
    next.push_str(&source[..start_idx]);
    next.push_str(&managed);
    next.push_str(&source[end_idx + end.len()..]);
    if next != source {
        write_if_changed(path, &next);
    }
}

fn join_fragments(fragments: &[String]) -> String {
    let mut out = String::new();
    for frag in fragments {
        out.push_str(frag.trim_end_matches('\n'));
        out.push('\n');
    }
    out
}

fn emit_kotlin_registry(android_root: &Path, package: &str, entries: &[RegistryEntry]) {
    let pkg_path = package.replace('.', "/");
    let dest = android_root
        .join("app/src/main/java")
        .join(pkg_path)
        .join("IstmoPluginRegistry.kt");
    let src = generate_kotlin_plugin_registry(package, entries);
    write_if_changed(&dest, &src);
}

fn emit_swift_registry(
    ios_root: &Path,
    app_dir: &str,
    plugins_subdir: &str,
    entries: &[RegistryEntry],
) {
    let dest = ios_root
        .join(app_dir)
        .join(plugins_subdir)
        .join("IstmoPluginRegistry.swift");
    let src = generate_swift_plugin_registry(entries);
    write_if_changed(&dest, &src);
}

fn cargo_manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set (build.rs only)"),
    )
}

fn emit_kotlin(
    android_root: &Path,
    package: &str,
    contract: &Contract,
    role: Role,
    seed_backends: bool,
) {
    let full_package = package.to_owned();
    let pkg_path = full_package.replace('.', "/");
    let gen_dir = android_root.join("app/src/main/java").join(pkg_path);
    let ty = &contract.type_name;

    match role {
        Role::Host => {
            write_if_changed(
                &gen_dir.join(format!("{ty}Dispatcher.kt")),
                &kotlin_with_package(&full_package, &generate_kotlin_host(contract)),
            );
            if seed_backends {
                seed_kotlin_backend_impl(&gen_dir, &full_package, contract);
            }
        }
        Role::Client => {
            write_if_changed(
                &gen_dir.join(format!("{ty}Codecs.kt")),
                &kotlin_with_package(&full_package, &generate_kotlin_codecs_interface(contract)),
            );
            write_if_changed(
                &gen_dir.join(format!("{ty}Client.kt")),
                &kotlin_with_package(&full_package, &generate_kotlin_client(contract)),
            );
        }
    }
    write_if_changed(
        &gen_dir.join(format!("{ty}Types.kt")),
        &kotlin_with_package(&full_package, &generate_kotlin_types(contract)),
    );
    write_if_changed(
        &gen_dir.join(format!("{ty}CodecsImpl.kt")),
        &kotlin_with_package(&full_package, &generate_kotlin_codecs(contract)),
    );
}

fn emit_swift(
    ios_root: &Path,
    app_dir: &str,
    plugins_subdir: &str,
    contract: &Contract,
    role: Role,
    seed_backends: bool,
) {
    let ty = &contract.type_name;
    let generated = ios_root
        .join(app_dir)
        .join(plugins_subdir)
        .join(ty)
        .join("Generated");
    match role {
        Role::Host => {
            write_if_changed(
                &generated.join(format!("{ty}Dispatcher.swift")),
                &generate_swift_host(contract),
            );
            if seed_backends {
                seed_swift_backend_impl(&generated, contract);
            }
        }
        Role::Client => {
            write_if_changed(
                &generated.join(format!("{ty}Client.swift")),
                &generate_swift_client(contract),
            );
        }
    }
    write_if_changed(
        &generated.join(format!("{ty}Types.swift")),
        &generate_swift_types(contract),
    );
    write_if_changed(
        &generated.join(format!("{ty}CodecsImpl.swift")),
        &generate_swift_codecs(contract),
    );
}

fn kotlin_with_package(package: &str, body: &str) -> String {
    const RUNTIME_PACKAGE: &str = "dev.istmo.runtime";
    let coroutine_imports = "import kotlinx.coroutines.flow.map\n";
    if package == RUNTIME_PACKAGE {
        return format!("package {package}\n\n{coroutine_imports}\n{body}");
    }
    let imports = "\
import dev.istmo.runtime.BackendException\n\
import dev.istmo.runtime.Bincode\n\
import dev.istmo.runtime.HandleReleaser\n\
import dev.istmo.runtime.IstmoRuntime\n\
import dev.istmo.runtime.PluginException\n\
import dev.istmo.runtime.PluginHandler\n\
import dev.istmo.runtime.PluginResult\n";
    format!("package {package}\n\n{imports}{coroutine_imports}\n{body}")
}

/// Every istmo plugin's `native/ios/` directory advertised through the
/// `DEP_*_ISTMO_NATIVE_IOS` env-var handover, as `(links-name, dir)`
/// pairs sorted by name so generated output is deterministic.
fn dep_native_ios_dirs() -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = std::env::vars()
        .filter_map(|(k, v)| {
            let stripped = k.strip_prefix("DEP_")?.strip_suffix("_ISTMO_NATIVE_IOS")?;
            Some((stripped.to_owned(), v))
        })
        .collect();
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    entries
}

/// Write a small `istmo-plugins.yml` xcodegen fragment into the
/// consumer's iOS app directory. Consumers' `project.yml` picks it up
/// via `include: [ istmo-plugins.yml ]` and merges the extra `sources:`
/// entries into their app target.
///
/// When any plugin declares `[min_versions] ios`, the fragment also
/// adds a pre-build script that fails the Xcode build if
/// `IPHONEOS_DEPLOYMENT_TARGET` is older — the Xcode-side twin of the
/// check `istmo-build` runs from Cargo.
///
/// The Android equivalent — auto-injecting Kotlin source dirs into
/// `sourceSets["main"]` — is handled by the standalone Gradle plugin
/// `dev.istmo:istmo-plugin-loader` on the runtime side; xcodegen has
/// nothing equivalent to a Gradle plugin, so we emit a fragment file
/// instead.
fn emit_ios_plugins_fragment(
    ios_root: &Path,
    app_dir: &str,
    entries: &[(String, String)],
    ios_min: Option<&(OsVersion, String)>,
) {
    if entries.is_empty() {
        return;
    }

    let fragment_dir = ios_root.join(app_dir);
    let mut yaml = String::new();
    yaml.push_str("# GENERATED by istmo-build — DO NOT EDIT.\n");
    yaml.push_str("# Include this file from your app's `project.yml`:\n");
    yaml.push_str("#   include:\n");
    yaml.push_str(&format!("#     - path: {app_dir}/istmo-plugins.yml\n"));
    yaml.push_str("# xcodegen merges the entries below into your app target.\n");
    yaml.push_str("targets:\n");
    yaml.push_str(&format!("  {app_dir}:\n"));
    yaml.push_str("    sources:\n");
    for (name, dir) in entries {
        // xcodegen resolves `path:` relative to the file that
        // declares it. Prefer a repo-relative path so the fragment is
        // portable across machines / CI runners; fall back to the
        // absolute path if the plugin lives outside the workspace.
        let display = relativise(&fragment_dir, Path::new(dir)).unwrap_or_else(|| dir.clone());
        yaml.push_str(&format!("      - path: \"{display}\"\n"));
        yaml.push_str(&format!("        name: {name}\n"));
        yaml.push_str("        type: group\n");
        yaml.push_str("        createIntermediateGroups: true\n");
        yaml.push_str("        excludes:\n");
        yaml.push_str("          - \"*.fragment\"\n");
    }
    if let Some((min, plugin)) = ios_min {
        yaml.push_str("    preBuildScripts:\n");
        yaml.push_str("      - name: istmo minimum iOS version\n");
        yaml.push_str("        basedOnDependencyAnalysis: false\n");
        yaml.push_str("        script: |\n");
        yaml.push_str(&format!("          required=\"{min}\"\n"));
        yaml.push_str("          actual=\"${IPHONEOS_DEPLOYMENT_TARGET:-0}\"\n");
        yaml.push_str(
            "          lowest=$(printf '%s\\n%s\\n' \"$required\" \"$actual\" | sort -t. -k1,1n -k2,2n -k3,3n | head -n1)\n",
        );
        yaml.push_str("          if [ \"$lowest\" != \"$required\" ]; then\n");
        yaml.push_str(&format!(
            "            echo \"error: istmo plugin {plugin} requires iOS >= $required, but IPHONEOS_DEPLOYMENT_TARGET is $actual\"\n"
        ));
        yaml.push_str("            exit 1\n");
        yaml.push_str("          fi\n");
    }

    let dest = fragment_dir.join("istmo-plugins.yml");
    write_if_changed(&dest, &yaml);
}

/// Merge every plugin's `Info.plist.fragment` / `App.entitlements.fragment`
/// and write them into the app (see [`crate::apple_plist`]).
fn emit_ios_plist_fragments(ios_root: &Path, app_dir: &str, entries: &[(String, String)]) {
    let app_path = ios_root.join(app_dir);
    let info_plist = app_path.join("Info.plist");
    merge_plist_fragments(
        entries,
        INFO_PLIST_FRAGMENT,
        &info_plist,
        &app_path.join("Info.plist.plugins.xml"),
    );
    let entitlements = find_entitlements(&app_path)
        .unwrap_or_else(|| app_path.join(format!("{app_dir}.entitlements")));
    let sidecar = entitlements.with_extension("entitlements.plugins.xml");
    merge_plist_fragments(entries, ENTITLEMENTS_FRAGMENT, &entitlements, &sidecar);
}

fn find_entitlements(app_path: &Path) -> Option<PathBuf> {
    let mut found: Vec<PathBuf> = fs::read_dir(app_path)
        .ok()?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "entitlements"))
        .collect();
    found.sort();
    found.into_iter().next()
}

fn merge_plist_fragments(
    entries: &[(String, String)],
    fragment_name: &str,
    target: &Path,
    sidecar: &Path,
) {
    let mut fragments = PlistFragments::default();
    for (name, dir) in entries {
        let path = Path::new(dir).join(fragment_name);
        if !path.is_file() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        if let Err(err) = fragments.add_file(name, &path) {
            println!("cargo::warning=istmo-build: {err}");
        }
    }
    if fragments.is_empty() {
        return;
    }
    if let Ok(source) = fs::read_to_string(target) {
        fragments.remove_app_keys(&app_keys_outside_block(&source));
    }
    for warning in fragments.warnings() {
        println!(
            "cargo::warning=istmo-build: {}: {warning}",
            target.display()
        );
    }
    let body = match fragments.render_body() {
        Ok(body) => body,
        Err(err) => {
            println!("cargo::warning=istmo-build: {err}");
            return;
        }
    };
    // Marker labels without their `<!-- -->` wrapper: XML comments do
    // not nest.
    let (start, end) = (
        marker_label(PLUGINS_MARK_START),
        marker_label(PLUGINS_MARK_END),
    );
    write_if_changed(
        sidecar,
        &format!(
            "<!-- GENERATED by istmo-build - DO NOT EDIT. -->\n\
             <!-- Copy the entries below into the <dict> of {}, or add the -->\n\
             <!-- comment markers `{start}` / `{end}` inside it -->\n\
             <!-- so istmo-build keeps the file in sync automatically. -->\n\
             {body}",
            target.display()
        ),
    );
    patch_marker_file(target, PLUGINS_MARK_START, PLUGINS_MARK_END, &[body]);
}

/// Return `target` expressed relative to `base` when both share a
/// prefix, using `..` segments for the ascent. Returns `None` when the
/// paths cannot be canonicalised (missing entries) so the caller can
/// fall back to the absolute path.
fn relativise(base: &Path, target: &Path) -> Option<String> {
    let base = fs::canonicalize(base).ok()?;
    let target = fs::canonicalize(target).ok()?;
    let base_components: Vec<_> = base.components().collect();
    let target_components: Vec<_> = target.components().collect();
    let common = base_components
        .iter()
        .zip(target_components.iter())
        .take_while(|(a, b)| a == b)
        .count();
    let ups = base_components.len() - common;
    let mut out = PathBuf::new();
    for _ in 0..ups {
        out.push("..");
    }
    for comp in &target_components[common..] {
        out.push(comp);
    }
    Some(out.to_string_lossy().into_owned())
}

fn write_if_changed(dest: &Path, contents: &str) {
    let run = || -> std::io::Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        if let Ok(existing) = fs::read_to_string(dest) {
            if existing == contents {
                return Ok(());
            }
        }
        fs::write(dest, contents)
    };
    if let Err(err) = run() {
        println!(
            "cargo::warning=istmo-build: failed to write {}: {err}",
            dest.display()
        );
    }
}

fn write_if_absent(dest: &Path, contents: &str) {
    if dest.exists() {
        return;
    }
    let run = || -> std::io::Result<()> {
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(dest, contents)
    };
    if let Err(err) = run() {
        println!(
            "cargo::warning=istmo-build: failed to seed {}: {err}",
            dest.display()
        );
    }
}

fn seed_kotlin_backend_impl(gen_dir: &Path, package: &str, contract: &Contract) {
    let ty = &contract.type_name;
    let mut body = String::new();
    body.push_str(&format!("package {package}\n\n"));
    body.push_str(&format!(
        "// Seeded by istmo-build. Fill in each `TODO()` with your logic;\n\
         // regenerate is a no-op once this file exists.\n\n"
    ));
    body.push_str(&format!("class {ty}BackendImpl : {ty}Backend {{\n"));
    for method in &contract.methods {
        body.push_str("    override suspend fun ");
        body.push_str(&kotlin_method_name(&method.name));
        body.push('(');
        let args = method
            .args
            .iter()
            .map(|a| format!("{}: Any", kotlin_arg_name(&a.name)))
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&args);
        body.push_str("): Any {\n");
        body.push_str("        TODO(\"implement ");
        body.push_str(&method.name);
        body.push_str("\")\n    }\n");
    }
    body.push_str("}\n");
    write_if_absent(&gen_dir.join(format!("{ty}BackendImpl.kt")), &body);
}

fn seed_swift_backend_impl(gen_dir: &Path, contract: &Contract) {
    let ty = &contract.type_name;
    let mut body = String::new();
    body.push_str("import Foundation\n\n");
    body.push_str(
        "// Seeded by istmo-build. Fill in each `fatalError` with your\n\
         // logic; regenerate is a no-op once this file exists.\n\n",
    );
    body.push_str(&format!("final class {ty}BackendImpl: {ty}Backend {{\n"));
    for method in &contract.methods {
        body.push_str("    func ");
        body.push_str(&method.name);
        body.push('(');
        let args = method
            .args
            .iter()
            .map(|a| format!("{}: Any", a.name))
            .collect::<Vec<_>>()
            .join(", ");
        body.push_str(&args);
        body.push_str(") async throws -> Any {\n");
        body.push_str("        fatalError(\"implement ");
        body.push_str(&method.name);
        body.push_str("\")\n    }\n");
    }
    body.push_str("}\n");
    write_if_absent(&gen_dir.join(format!("{ty}BackendImpl.swift")), &body);
}

fn kotlin_method_name(rust_snake: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for ch in rust_snake.chars() {
        if ch == '_' {
            upper = true;
        } else if upper {
            out.extend(ch.to_uppercase());
            upper = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn kotlin_arg_name(rust_snake: &str) -> String {
    kotlin_method_name(rust_snake)
}

#[must_use]
pub fn detect_android_package(android_root: &Path) -> Option<String> {
    for name in ["build.gradle.kts", "build.gradle"] {
        let path = android_root.join("app").join(name);
        if let Ok(text) = fs::read_to_string(&path) {
            if let Some(ns) = extract_namespace(&text) {
                return Some(ns);
            }
        }
    }
    let manifest = android_root.join("app/src/main/AndroidManifest.xml");
    if let Ok(text) = fs::read_to_string(&manifest) {
        if let Some(pkg) = extract_manifest_package(&text) {
            return Some(pkg);
        }
    }
    None
}

fn extract_namespace(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("namespace") {
            let rest = rest.trim_start();
            let rest = rest.strip_prefix('=')?.trim_start();
            let quoted = rest.strip_prefix('"').or_else(|| rest.strip_prefix('\''))?;
            let end = quoted.find(['"', '\''])?;
            return Some(quoted[..end].to_owned());
        }
    }
    None
}

fn extract_manifest_package(xml: &str) -> Option<String> {
    let idx = xml.find("package=")?;
    let after = &xml[idx + "package=".len()..];
    let quoted = after
        .strip_prefix('"')
        .or_else(|| after.strip_prefix('\''))?;
    let end = quoted.find(['"', '\''])?;
    Some(quoted[..end].to_owned())
}

#[must_use]
pub fn detect_ios_app_dir(ios_root: &Path) -> Option<String> {
    let entries = fs::read_dir(ios_root).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !n.starts_with('.'))
                && !p
                    .extension()
                    .is_some_and(|e| e == "xcodeproj" || e == "xcworkspace")
        })
        .collect();

    if candidates.len() > 1 {
        candidates.retain(|dir| {
            let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
            ios_root.join(format!("{name}.xcodeproj")).exists()
                || ios_root.join(format!("{name}.xcworkspace")).exists()
        });
    }

    let picked = if candidates.len() == 1 {
        candidates.into_iter().next()
    } else {
        None
    }?;
    picked
        .file_name()
        .and_then(|n| n.to_str())
        .map(str::to_owned)
}

#[derive(Debug, Clone, Default)]
struct AppConfig {
    android: Option<bool>,
    android_package: Option<String>,
    ios: Option<bool>,
    ios_app_dir: Option<String>,
    ios_plugins_subdir: Option<String>,
    auto_register: Option<bool>,
    per_plugin: HashMap<String, AppPluginOpts>,
    info_plist: Vec<InfoPlistEntry>,
}

fn parse_app_config(doc: &DocumentMut) -> AppConfig {
    let mut cfg = AppConfig::default();
    let Some(app_item) = doc.get("app") else {
        return cfg;
    };
    let Some(app_table) = app_item.as_table() else {
        println!("cargo::warning=istmo-build: `[app]` expected table");
        return cfg;
    };
    if let Some(v) = app_table.get("android").and_then(item_bool) {
        cfg.android = Some(v);
    }
    if let Some(v) = app_table.get("android_package").and_then(item_str) {
        cfg.android_package = Some(v.to_owned());
    }
    if let Some(v) = app_table.get("ios").and_then(item_bool) {
        cfg.ios = Some(v);
    }
    if let Some(v) = app_table.get("ios_app_dir").and_then(item_str) {
        cfg.ios_app_dir = Some(v.to_owned());
    }
    if let Some(v) = app_table.get("ios_plugins_subdir").and_then(item_str) {
        cfg.ios_plugins_subdir = Some(v.to_owned());
    }
    if let Some(v) = app_table.get("auto_register").and_then(item_bool) {
        cfg.auto_register = Some(v);
    }
    if let Some(table) = app_table.get("info_plist").and_then(Item::as_table_like) {
        match InfoPlistEntry::parse_table(table, "app.info_plist") {
            Ok(entries) => cfg.info_plist = entries,
            Err(err) => println!("cargo::warning=istmo-build: {err}"),
        }
    }
    if let Some(item) = app_table.get("plugin") {
        if let Some(array) = item.as_array_of_tables() {
            for entry in array {
                if let Some((key, opts)) = parse_plugin_entry(entry) {
                    cfg.per_plugin.insert(key, opts);
                }
            }
        } else if let Some(table) = item.as_table() {
            if let Some((key, opts)) = parse_plugin_entry(table) {
                cfg.per_plugin.insert(key, opts);
            }
        }
    }
    cfg
}

fn parse_plugin_entry(table: &Table) -> Option<(String, AppPluginOpts)> {
    let key = table
        .get("id")
        .and_then(item_str)
        .or_else(|| table.get("type").and_then(item_str))?;
    let role = table.get("role").and_then(item_str).and_then(Role::parse);
    let platforms = table.get("platforms").and_then(|item| {
        let array = item.as_array()?;
        let mut out = Vec::new();
        for v in array {
            if let Value::String(s) = v {
                if let Some(p) = Platform::parse(s.value().as_str()) {
                    out.push(p);
                }
            }
        }
        Some(out)
    });
    let auto_register = table.get("auto_register").and_then(item_bool);
    Some((
        key.to_owned(),
        AppPluginOpts {
            role,
            platforms,
            auto_register,
        },
    ))
}

fn item_str(item: &Item) -> Option<&str> {
    match item {
        Item::Value(Value::String(s)) => Some(s.value().as_str()),
        _ => None,
    }
}

fn item_bool(item: &Item) -> Option<bool> {
    match item {
        Item::Value(Value::Boolean(b)) => Some(*b.value()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_namespace_from_kts() {
        let src = "android {\n    namespace = \"dev.istmo.demo\"\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("dev.istmo.demo"));
    }

    #[test]
    fn extracts_namespace_from_groovy_single_quote() {
        let src = "android {\n    namespace 'dev.istmo.demo'\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), None);
        let src = "android {\n    namespace = 'dev.istmo.demo'\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("dev.istmo.demo"));
    }

    #[test]
    fn extracts_namespace_with_extra_spaces() {
        let src = "android {\n   namespace   =    \"a.b.c\"\n}\n";
        assert_eq!(extract_namespace(src).as_deref(), Some("a.b.c"));
    }

    #[test]
    fn extracts_manifest_package() {
        let xml = r#"<manifest xmlns:android="http://schemas.android.com/apk/res/android" package="dev.istmo.legacy">"#;
        assert_eq!(
            extract_manifest_package(xml).as_deref(),
            Some("dev.istmo.legacy")
        );
    }

    #[test]
    fn parses_app_section_full() {
        let toml = r#"
[app]
android_package = "com.example.app.generated"
ios_app_dir = "MyApp"

[[app.plugin]]
id = "istmo.echo"
role = "client"
platforms = ["android"]
"#;
        let doc: DocumentMut = toml.parse().unwrap();
        let cfg = parse_app_config(&doc);
        assert_eq!(
            cfg.android_package.as_deref(),
            Some("com.example.app.generated")
        );
        assert_eq!(cfg.ios_app_dir.as_deref(), Some("MyApp"));
        let per = cfg.per_plugin.get("istmo.echo").unwrap();
        assert_eq!(per.role, Some(Role::Client));
        assert_eq!(per.platforms.as_deref(), Some(&[Platform::Android][..]));
    }

    #[test]
    fn parses_platform_flags() {
        let toml = "[app]\nandroid = false\nios = true\n";
        let doc: DocumentMut = toml.parse().unwrap();
        let cfg = parse_app_config(&doc);
        assert_eq!(cfg.android, Some(false));
        assert_eq!(cfg.ios, Some(true));
    }

    #[test]
    fn kotlin_method_name_snake_to_camel() {
        assert_eq!(kotlin_method_name("sign_in"), "signIn");
        assert_eq!(kotlin_method_name("open_url"), "openUrl");
        assert_eq!(kotlin_method_name("echo"), "echo");
    }
}
