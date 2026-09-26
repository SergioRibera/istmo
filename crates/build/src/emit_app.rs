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

use crate::android_project::{AndroidPlugin, AndroidProject};
use crate::app_config::{AppConfig, AppMetadata, AppPluginOpts, CrateInfo, Platform, Role};
use crate::app_icon::AppIcon;
use crate::apple_plist::{
    ENTITLEMENTS_FRAGMENT, INFO_PLIST_FRAGMENT, PLUGINS_MARK_END, PLUGINS_MARK_START,
    PlistFragments, app_keys_outside_block,
};
use crate::contract::Contract;
use crate::doctor::Doctor;
use crate::handover::{
    NativePlatform, collect_dep_contracts, collect_dep_manifests_by_links, collect_dep_native_deps,
    collect_dep_native_dirs,
};
use crate::ios::{BackgroundKind, ContinuousMode, IosBackgroundContract, generate_ios_background};
use crate::ios_project::IosProject;
use crate::kotlin_client::generate_kotlin_client;
use crate::kotlin_host::{generate_kotlin_codecs_interface, generate_kotlin_host};
use crate::kotlin_types::{generate_kotlin_codecs, generate_kotlin_types};
use crate::manifest::{
    AndroidServiceSpec, InfoPlistEntry, IosBackgroundKindSpec, IosBackgroundSpec,
    IosContinuousModeSpec, Manifest, PluginEntry,
};
use crate::plugin_registry::{
    KOTLIN_REGISTRY_PACKAGE, RegistryEntry, generate_kotlin_plugin_registry,
    generate_swift_app_entry, generate_swift_plugin_registry,
};
use crate::service::{ServiceContract, generate_android_service};
use crate::swift::generate_swift_client;
use crate::swift_host::generate_swift_host;
use crate::swift_types::{generate_swift_codecs, generate_swift_types};

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
    let app_config = AppConfig::from_path(&manifest_path)
        .unwrap_or_else(|err| panic!("istmo-build: {}: {err}", manifest_path.display()));

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

    let mut per_plugin = app_config.per_plugin.clone();
    for (k, v) in opts.per_plugin.clone() {
        per_plugin.insert(k, v);
    }

    let mut contracts = collect_dep_contracts();
    contracts.extend(opts.extra_contracts.iter().cloned());

    let dep_manifests_by_links = collect_dep_manifests_by_links();
    let mut dep_manifests: Vec<Manifest> = dep_manifests_by_links
        .iter()
        .map(|(_, manifest)| manifest.clone())
        .collect();
    dep_manifests.extend(opts.extra_manifests.iter().cloned());
    let plugin_entries: HashMap<&str, &PluginEntry> = dep_manifests
        .iter()
        .flat_map(|m| m.plugins.iter())
        .map(|p| (p.id.as_str(), p))
        .collect();

    let app_manifest = manifest_path.is_file().then(|| {
        Manifest::from_path(&manifest_path)
            .unwrap_or_else(|err| panic!("istmo-build: {}: {err}", manifest_path.display()))
    });
    let app = resolve_app_metadata(
        &root,
        &app_config,
        app_manifest.as_ref(),
        opts.lib_name.as_ref(),
    );
    let rust_entry = app_config
        .rust_entry
        .unwrap_or_else(|| app.krate.declares_mobile_app());
    let icon = app.icon.as_ref().map(|path| {
        println!("cargo:rerun-if-changed={}", path.display());
        AppIcon::open(path).unwrap_or_else(|err| panic!("istmo-build: `[app] icon`: {err}"))
    });

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

        let plugin_entry = plugin_entries.get(contract.plugin_id.as_str()).copied();
        let plugin_declared_auto = plugin_entry.is_none_or(|p| p.auto_register);
        let registry_entry =
            RegistryEntry::from_contract(contract).map(|entry| match plugin_entry {
                Some(plugin) => entry.with_plugin(plugin),
                None => entry,
            });
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
                        kotlin_registry.extend(registry_entry.clone());
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
                        swift_registry.extend(registry_entry.clone());
                    }
                }
            }
        }
    }

    // Registries are emitted even when empty: `IstmoActivity` and the
    // generated `IstmoApp.run()` reference them unconditionally.
    if android_active {
        match android_package.as_deref() {
            Some(pkg) => emit_kotlin_registry(&android_root, pkg, &kotlin_registry),
            None if kotlin_registry.is_empty() => {
                emit_kotlin_registry(&android_root, KOTLIN_REGISTRY_PACKAGE, &[]);
            }
            None => {}
        }
        if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "android") {
            sync_android_project(
                &android_root,
                &app,
                icon.as_ref(),
                &dep_manifests_by_links,
                app_manifest.as_ref(),
            );
        }
    }
    if ios_active {
        if let Some(app_dir) = ios_app_dir.as_deref() {
            let mut native_dirs = collect_dep_native_dirs(NativePlatform::Ios);
            native_dirs.extend(opts.extra_native_ios_dirs.iter().cloned());
            sync_ios_project(
                &IosProject::new(&ios_root, app_dir),
                &IosSync {
                    app: &app,
                    icon: icon.as_ref(),
                    rust_entry,
                    plugins_subdir: &ios_plugins_subdir,
                    registry: &swift_registry,
                    native_dirs: &native_dirs,
                    dep_manifests: &dep_manifests,
                },
            );
        }
    }

    let lib_name = app.krate.lib_name.clone();

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

    Doctor::new(
        &app,
        android_active.then_some(android_root.as_path()),
        ios_app_dir
            .as_deref()
            .filter(|_| ios_active)
            .map(|dir| (ios_root.as_path(), dir)),
    )
    .report();

    println!("cargo:rerun-if-changed=build.rs");
    if manifest_path.exists() {
        println!("cargo:rerun-if-changed=istmo.toml");
    }
}

fn resolve_app_metadata(
    root: &Path,
    config: &AppConfig,
    app_manifest: Option<&Manifest>,
    lib_name: Option<&String>,
) -> AppMetadata {
    let mut krate = CrateInfo::from_build_env(root);
    if let Some(lib_name) = lib_name {
        krate.lib_name.clone_from(lib_name);
    }
    let min_versions = app_manifest.map(|m| m.min_versions).unwrap_or_default();
    AppMetadata::resolve(&config.identity, &min_versions, krate)
}

/// Write `android/.istmo/` for the `dev.istmo.app` Gradle plugin.
fn sync_android_project(
    android_root: &Path,
    app: &AppMetadata,
    icon: Option<&AppIcon>,
    dep_manifests_by_links: &[(String, Manifest)],
    app_manifest: Option<&Manifest>,
) {
    let project = AndroidProject::new(android_root);
    let plugins = AndroidPlugin::link(
        &collect_dep_native_dirs(NativePlatform::Android),
        dep_manifests_by_links,
    );
    let mut deps = collect_dep_native_deps();
    if let Some(manifest) = app_manifest {
        deps.merge(manifest.native_deps.clone());
    }
    for conflict in deps.conflicts() {
        println!(
            "cargo::warning=istmo-build: Gradle dependency {}:{} requested at several versions; \
             using {} (dropped {})",
            conflict.key.group,
            conflict.key.artifact,
            conflict.picked,
            conflict.discarded.join(", "),
        );
    }
    write_if_changed(
        &project.metadata_path(),
        &project.render_metadata(app, &plugins, &deps),
    );
    if let Some(icon) = icon {
        project
            .write_icon(icon)
            .unwrap_or_else(|err| panic!("istmo-build: {err}"));
    }
}

struct IosSync<'a> {
    app: &'a AppMetadata,
    icon: Option<&'a AppIcon>,
    rust_entry: bool,
    plugins_subdir: &'a str,
    registry: &'a [RegistryEntry],
    native_dirs: &'a [(String, PathBuf)],
    dep_manifests: &'a [Manifest],
}

/// Write the iOS side: plugin registry, `IstmoApp.run()` entry,
/// `ios/.istmo/`, the xcodegen fragment and the merged plist fragments.
fn sync_ios_project(project: &IosProject, sync: &IosSync<'_>) {
    let plugins_dir = project.app_path().join(sync.plugins_subdir);
    write_if_changed(
        &plugins_dir.join("IstmoPluginRegistry.swift"),
        &generate_swift_plugin_registry(sync.registry),
    );
    let entry = plugins_dir.join("IstmoMain.swift");
    if sync.rust_entry {
        write_if_changed(&entry, &generate_swift_app_entry());
    } else {
        remove_generated(&entry);
    }

    project.write_generated(sync.app);
    if let Some(icon) = sync.icon {
        project
            .write_icon(icon)
            .unwrap_or_else(|err| panic!("istmo-build: {err}"));
    }
    let plugin_min_ios = sync
        .dep_manifests
        .iter()
        .filter_map(|m| m.min_versions.ios.map(|v| (v, m.primary_id().to_owned())))
        .max_by_key(|(v, _)| *v);
    project.write_xcodegen_fragment(sync.app, sync.native_dirs, plugin_min_ios.as_ref());
    emit_ios_plist_fragments(project.root(), project.app_dir(), sync.native_dirs);
}

/// Delete a file istmo-build generated earlier and no longer emits.
/// Files without the generated header are left alone.
fn remove_generated(path: &Path) {
    if fs::read_to_string(path).is_ok_and(|text| text.contains("GENERATED by istmo-build")) {
        let _ = fs::remove_file(path);
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

fn emit_kotlin_registry(android_root: &Path, codegen_package: &str, entries: &[RegistryEntry]) {
    let java_root = android_root.join("app/src/main/java");
    let dest = java_root
        .join(KOTLIN_REGISTRY_PACKAGE.replace('.', "/"))
        .join("IstmoPluginRegistry.kt");
    write_if_changed(
        &dest,
        &generate_kotlin_plugin_registry(codegen_package, entries),
    );
    // Before 0.2 the registry lived in the codegen package.
    let legacy = java_root
        .join(codegen_package.replace('.', "/"))
        .join("IstmoPluginRegistry.kt");
    if legacy != dest {
        remove_generated(&legacy);
    }
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

/// Merge every plugin's `Info.plist.fragment` / `App.entitlements.fragment`
/// and write them into the app (see [`crate::apple_plist`]).
fn emit_ios_plist_fragments(ios_root: &Path, app_dir: &str, entries: &[(String, PathBuf)]) {
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
    entries: &[(String, PathBuf)],
    fragment_name: &str,
    target: &Path,
    sidecar: &Path,
) {
    let mut fragments = PlistFragments::default();
    for (name, dir) in entries {
        let path = dir.join(fragment_name);
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
pub(crate) fn relativise(base: &Path, target: &Path) -> Option<String> {
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

pub(crate) fn write_if_changed(dest: &Path, contents: &str) {
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
    fn kotlin_method_name_snake_to_camel() {
        assert_eq!(kotlin_method_name("sign_in"), "signIn");
        assert_eq!(kotlin_method_name("open_url"), "openUrl");
        assert_eq!(kotlin_method_name("echo"), "echo");
    }
}
