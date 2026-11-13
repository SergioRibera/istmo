//! App-side pass: `istmo_build::emit_app()` turns every upstream
//! plugin's `Contract` into Kotlin / Swift sources inside `android/`
//! and `ios/`.
//!
//! It walks the contracts handed over through [`collect_dep_contracts`]
//! and writes matching dispatcher, types and codecs files into
//! conventional paths, then syncs the native projects: permissions,
//! services, plist fragments, assets, icons and metadata.
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
//!
//! Submodules: `codegen` (Kotlin / Swift sources and registries),
//! `native_patch` (manifest / `Info.plist` sidecars and marker
//! patches), `assets` (asset staging) and `detect` (project layout
//! detection).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::android_project::{AndroidPlugin, AndroidProject};
use crate::app_config::{AppConfig, AppMetadata, AppPluginOpts, CrateInfo, Platform, Role};
use crate::app_icon::{AppIcon, IconError};
use crate::contract::Contract;
use crate::doctor::Doctor;
use crate::handover::{
    NativePlatform, collect_dep_contracts, collect_dep_manifests_by_links, collect_dep_native_deps,
    collect_dep_native_dirs,
};
use crate::ios_project::IosProject;
use crate::manifest::{IstmoToml, Manifest, PluginEntry};
use crate::plugin_registry::{
    KOTLIN_REGISTRY_PACKAGE, RegistryEntry, generate_swift_app_entry,
    generate_swift_plugin_registry,
};
use crate::windows_resources::{WindowsResources, WindowsToolchain, compose_manifest};

mod assets;
mod codegen;
mod detect;
mod native_patch;

use assets::sync_assets;
use codegen::{emit_kotlin, emit_kotlin_registry, emit_swift, mark_codegen_dir};
pub use detect::{detect_android_package, detect_ios_app_dir};
use native_patch::{
    ServiceEmitOpts, emit_ios_plist_fragments, emit_service_and_background,
    emit_wired_android_permissions, resolve_wired_plugin_entries,
};

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
    /// Contracts no dependency hands over, such as ones built inline in
    /// the app's `build.rs`. One with the plugin id of a handed-over
    /// contract replaces it.
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

/// Window icon staged in `OUT_DIR` for `istmo::app_icon!()`.
const DESKTOP_ICON_PNG: &str = "istmo-app-icon.png";
/// Large enough for `HiDPI` title bars and the Windows taskbar.
const DESKTOP_ICON_EDGE: u32 = 256;

/// # Panics
///
/// When `istmo.toml` or the `[app]` configuration is invalid — see the
/// crate-level error policy.
#[expect(
    clippy::needless_pass_by_value,
    reason = "public signature kept for existing build scripts"
)]
pub fn emit_app_with(opts: AppOpts) {
    let root = opts.root.clone().unwrap_or_else(cargo_manifest_dir);
    let toml = IstmoToml::load(&root.join("istmo.toml"));
    emit_app_from(&opts, &root, &toml);
}

/// App-side pass over an `istmo.toml` the caller already parsed.
pub(crate) fn emit_app_from(opts: &AppOpts, root: &Path, toml: &IstmoToml) {
    let pass = AppPass::resolve(opts, root, toml);
    track_crate_inputs(&pass.root);
    // Plugin crates run `emit()` too: only apps (a native project or a
    // binary to ship) get the desktop icon, runtime env and assets.
    if !pass.android_active && !pass.ios_active && !pass.app.krate.has_binaries() {
        return;
    }
    pass.track_inputs();
    emit_desktop_icon(&pass.app, &pass.dep_manifests_by_links, pass.app_manifest);
    // Desktop builds often ship without `android/` / `ios/` (e.g. a Nix
    // source filter), and still need the runtime env and staged assets.
    let desktop_staging = sync_assets(
        &pass.root,
        pass.app_manifest,
        pass.android_active.then_some(pass.android_root.as_path()),
        pass.active_ios_app_dir()
            .map(|dir| (pass.ios_root.as_path(), dir)),
    );
    emit_runtime_env(
        &pass.app,
        pass.config,
        &pass.root,
        desktop_staging.as_deref(),
    );
    if !pass.android_active && !pass.ios_active {
        return;
    }

    let android_package = pass.android_package();
    let mut dep_manifests: Vec<Manifest> = pass
        .dep_manifests_by_links
        .iter()
        .map(|(_, manifest)| manifest.clone())
        .collect();
    dep_manifests.extend(opts.extra_manifests.iter().cloned());
    let rust_entry = pass.config.rust_entry.unwrap_or_else(|| {
        // The scan reads every file under `src/`.
        crate::directive::rerun_if_exists(&pass.root.join("src"));
        pass.app.krate.declares_mobile_app()
    });
    let icon = pass.open_launcher_icon();

    let registries = pass.emit_contract_sources(android_package.as_deref(), &dep_manifests);
    if pass.android_active {
        pass.sync_android(
            android_package.as_deref(),
            &registries.kotlin,
            icon.as_ref(),
        );
    }
    if let Some(app_dir) = pass.active_ios_app_dir() {
        pass.sync_ios(
            app_dir,
            icon.as_ref(),
            rust_entry,
            &registries.swift,
            &dep_manifests,
        );
    }
    pass.emit_wired_surfaces(android_package.as_deref(), &dep_manifests);

    Doctor::new(
        &pass.app,
        pass.android_active.then_some(pass.android_root.as_path()),
        pass.active_ios_app_dir()
            .map(|dir| (pass.ios_root.as_path(), dir)),
    )
    .report();
}

/// Everything one app-side run resolves up front, shared by its steps.
struct AppPass<'a> {
    opts: &'a AppOpts,
    config: &'a AppConfig,
    app_manifest: Option<&'a Manifest>,
    root: PathBuf,
    android_root: PathBuf,
    ios_root: PathBuf,
    android_active: bool,
    ios_active: bool,
    ios_app_dir: Option<String>,
    /// Whether [`Self::ios_app_dir`] came from [`detect_ios_app_dir`].
    ios_app_dir_detected: bool,
    ios_plugins_subdir: String,
    app: AppMetadata,
    dep_manifests_by_links: Vec<(String, Manifest)>,
}

/// Where one contract's Kotlin goes.
#[derive(Debug, Clone, Copy)]
struct KotlinTarget<'a> {
    /// The app's codegen package (`[app] android_package`, else
    /// `<namespace>.gen`), when it resolves.
    app: Option<&'a str>,
    /// The plugin's own `[plugin] android_package`; wins over [`Self::app`].
    plugin: Option<&'a str>,
}

/// Plugin registrations collected while emitting contract sources.
#[derive(Debug, Default)]
struct Registries {
    kotlin: Vec<RegistryEntry>,
    swift: Vec<RegistryEntry>,
}

impl<'a> AppPass<'a> {
    fn resolve(opts: &'a AppOpts, root: &Path, toml: &'a IstmoToml) -> Self {
        let config = &toml.app;
        let app_manifest = toml.manifest.as_ref();
        let android_root = opts
            .android_root
            .clone()
            .unwrap_or_else(|| root.join("android"));
        let ios_root = opts.ios_root.clone().unwrap_or_else(|| root.join("ios"));
        let android_active =
            opts.android.or(config.android).unwrap_or(true) && android_root.is_dir();
        let ios_active = opts.ios.or(config.ios).unwrap_or(true) && ios_root.is_dir();
        let configured_ios_app_dir = opts
            .ios_app_dir
            .clone()
            .or_else(|| config.ios_app_dir.clone());
        let ios_app_dir_detected = configured_ios_app_dir.is_none();
        let ios_app_dir = configured_ios_app_dir.or_else(|| detect_ios_app_dir(&ios_root));
        let ios_plugins_subdir = opts
            .ios_plugins_subdir
            .clone()
            .or_else(|| config.ios_plugins_subdir.clone())
            .unwrap_or_else(|| "Plugins".to_owned());
        let app = resolve_app_metadata(root, config, app_manifest, opts.lib_name.as_ref());
        Self {
            opts,
            config,
            app_manifest,
            root: root.to_path_buf(),
            android_root,
            ios_root,
            android_active,
            ios_active,
            ios_app_dir,
            ios_app_dir_detected,
            ios_plugins_subdir,
            app,
            dep_manifests_by_links: collect_dep_manifests_by_links(),
        }
    }

    fn active_ios_app_dir(&self) -> Option<&str> {
        self.ios_app_dir.as_deref().filter(|_| self.ios_active)
    }

    /// Every linked `native/ios/` directory as `(links, dir)`.
    fn native_ios_dirs(&self) -> Vec<(String, PathBuf)> {
        let mut dirs = collect_dep_native_dirs(NativePlatform::Ios);
        dirs.extend(self.opts.extra_native_ios_dirs.iter().cloned());
        dirs
    }

    /// Ids of the plugins whose crate ships `native/ios/` sources.
    fn plugins_with_native_ios(&self) -> HashSet<&str> {
        let native = self.native_ios_dirs();
        self.dep_manifests_by_links
            .iter()
            .filter(|(links, _)| native.iter().any(|(name, _)| name == links))
            .flat_map(|(_, manifest)| manifest.plugins.iter().map(|p| p.id.as_str()))
            .collect()
    }

    /// Icon sources and the native project files the pass reads.
    fn track_inputs(&self) {
        for source in self.app.icon.source_files() {
            crate::directive::rerun_if_changed(source);
        }
        if self.android_active {
            track_android_inputs(&self.android_root);
        }
        if self.ios_active {
            track_ios_inputs(
                &self.ios_root,
                self.ios_app_dir.as_deref(),
                self.ios_app_dir_detected,
            );
        }
    }

    fn android_package(&self) -> Option<String> {
        self.opts
            .android_package
            .clone()
            .or_else(|| self.config.android_package.clone())
            .or_else(|| detect_android_package(&self.android_root).map(|ns| format!("{ns}.gen")))
    }

    /// Decode `[app.icon] image`; a broken source fails the build.
    fn open_launcher_icon(&self) -> Option<AppIcon> {
        let icon = &self.app.icon;
        if icon.image.is_none()
            && icon.android.is_some()
            && self.app.min_android.is_some_and(|sdk| sdk < 26)
        {
            crate::directive::warning(
                "`[app.icon.android]` only covers Android 8+ (API 26); \
                 set `[app.icon] image` so older devices get a launcher icon too",
            );
        }
        icon.image.as_ref().map(|path| {
            AppIcon::open(path)
                .unwrap_or_else(|err| panic!("istmo-build: `[app.icon] image`: {err}"))
        })
    }

    /// Kotlin / Swift sources for every plugin contract, collecting the
    /// plugins each platform's registry auto-registers.
    fn emit_contract_sources(
        &self,
        android_package: Option<&str>,
        dep_manifests: &[Manifest],
    ) -> Registries {
        let mut per_plugin = self.config.per_plugin.clone();
        per_plugin.extend(self.opts.per_plugin.clone());
        let plugin_entries: HashMap<&str, &PluginEntry> = dep_manifests
            .iter()
            .flat_map(|m| m.plugins.iter())
            .map(|p| (p.id.as_str(), p))
            .collect();
        let app_auto_register = self
            .opts
            .auto_register
            .or(self.config.auto_register)
            .unwrap_or(true);
        let contracts = with_extra_contracts(collect_dep_contracts(), &self.opts.extra_contracts);
        let ships_swift = self.plugins_with_native_ios();

        let mut registries = Registries::default();
        for contract in &contracts {
            let overrides = per_plugin
                .get(contract.plugin_id.as_str())
                .or_else(|| per_plugin.get(contract.type_name.as_str()));
            let role = overrides.and_then(|o| o.role).unwrap_or(Role::Host);
            let platforms = overrides
                .and_then(|o| o.platforms.clone())
                .unwrap_or_else(|| self.active_platforms());
            let plugin_entry = plugin_entries.get(contract.plugin_id.as_str()).copied();
            let registry_entry =
                RegistryEntry::from_contract(contract).map(|entry| match plugin_entry {
                    Some(plugin) => entry.with_plugin(plugin),
                    None => entry,
                });
            let auto_register = app_auto_register
                && plugin_entry.is_none_or(|p| p.auto_register)
                && overrides.and_then(|o| o.auto_register).unwrap_or(true)
                && role == Role::Host;

            let plugin_package = plugin_entry.and_then(|p| p.android_package.as_deref());
            for platform in &platforms {
                let emitted = match platform {
                    Platform::Android => self.emit_kotlin_sources(
                        KotlinTarget {
                            app: android_package,
                            plugin: plugin_package,
                        },
                        contract,
                        role,
                    ),
                    Platform::Ios => self.emit_swift_sources(
                        contract,
                        role,
                        ships_swift.contains(contract.plugin_id.as_str()),
                    ),
                };
                if let Some(entry) = registry_entry.as_ref().filter(|_| emitted && auto_register) {
                    match platform {
                        Platform::Android => registries.kotlin.push(entry.clone()),
                        Platform::Ios => registries.swift.push(entry.clone()),
                    }
                }
            }
        }
        registries
    }

    fn active_platforms(&self) -> Vec<Platform> {
        let mut platforms = Vec::new();
        if self.android_active {
            platforms.push(Platform::Android);
        }
        if self.ios_active {
            platforms.push(Platform::Ios);
        }
        platforms
    }

    /// Returns whether sources were written.
    fn emit_kotlin_sources(
        &self,
        target: KotlinTarget<'_>,
        contract: &Contract,
        role: Role,
    ) -> bool {
        if !self.android_active {
            return false;
        }
        let java_root = self.android_root.join("app/src/main/java");
        let Some(plugin_package) = target.plugin else {
            let Some(pkg) = target.app else {
                crate::directive::warning(&format!(
                    "cannot resolve Android namespace for `{}`; set `[app] android_package` in istmo.toml",
                    contract.type_name
                ));
                return false;
            };
            emit_kotlin(&java_root, pkg, contract, role, self.opts.seed_backends);
            return true;
        };
        // The plugin's own `native/android/` sources implement the backend
        // in this package: a seeded `<T>BackendImpl` would clash with it.
        emit_kotlin(&java_root, plugin_package, contract, role, false);
        mark_codegen_dir(&java_root.join(plugin_package.replace('.', "/")));
        true
    }

    /// Returns whether sources were written.
    /// `ships_backend`: the plugin's `native/ios/` sources implement the
    /// backend, so a seeded `<T>BackendImpl` would redeclare it.
    fn emit_swift_sources(&self, contract: &Contract, role: Role, ships_backend: bool) -> bool {
        if !self.ios_active {
            return false;
        }
        let Some(app_dir) = self.ios_app_dir.as_deref() else {
            crate::directive::warning(&format!(
                "cannot resolve iOS app dir for `{}`; set `[app] ios_app_dir` in istmo.toml",
                contract.type_name
            ));
            return false;
        };
        emit_swift(
            &self.ios_root,
            app_dir,
            &self.ios_plugins_subdir,
            contract,
            role,
            self.opts.seed_backends && !ships_backend,
        );
        true
    }

    /// Kotlin registry, the default package's ignore marker and, when
    /// compiling for Android, `android/.istmo/`.
    fn sync_android(
        &self,
        android_package: Option<&str>,
        registry: &[RegistryEntry],
        icon: Option<&AppIcon>,
    ) {
        // The default `<namespace>.gen` package is istmo-build's alone; an
        // explicit `android_package` may be shared with hand-written code.
        let package_is_default =
            self.opts.android_package.is_none() && self.config.android_package.is_none();
        if let Some(pkg) = android_package.filter(|_| package_is_default) {
            mark_codegen_dir(
                &self
                    .android_root
                    .join("app/src/main/java")
                    .join(pkg.replace('.', "/")),
            );
        }
        // Registries are emitted even when empty: `IstmoHost` and the
        // generated `IstmoApp.run()` reference them unconditionally.
        // Without an app package every listed plugin brought its own
        // `[plugin] android_package`, so the fallback is never named.
        emit_kotlin_registry(
            &self.android_root,
            android_package.unwrap_or(KOTLIN_REGISTRY_PACKAGE),
            registry,
        );
        // Set by the `io.github.sergioribera.istmo` Gradle plugin to force a rewrite of
        // `android/.istmo/istmo.json` when it bootstraps the metadata.
        crate::directive::rerun_if_env_changed("ISTMO_METADATA_REFRESH");
        if std::env::var("CARGO_CFG_TARGET_OS").is_ok_and(|os| os == "android") {
            sync_android_project(
                &self.android_root,
                &self.app,
                icon,
                &self.dep_manifests_by_links,
                self.app_manifest,
            );
        }
    }

    /// Swift registry, `IstmoApp.run()` entry, `ios/.istmo/`, the
    /// xcodegen fragment and the merged plist fragments.
    fn sync_ios(
        &self,
        app_dir: &str,
        icon: Option<&AppIcon>,
        rust_entry: bool,
        registry: &[RegistryEntry],
        dep_manifests: &[Manifest],
    ) {
        let native_dirs = self.native_ios_dirs();
        let project = IosProject::new(&self.ios_root, app_dir);
        // Pruning keeps the staging root itself; an emptied one is not shipped.
        let asset_folder = Some(project.asset_folder_path())
            .filter(|dir| fs::read_dir(dir).is_ok_and(|mut entries| entries.next().is_some()));
        sync_ios_project(
            &project,
            &IosSync {
                app: &self.app,
                icon,
                rust_entry,
                plugins_subdir: &self.ios_plugins_subdir,
                registry,
                native_dirs: &native_dirs,
                dep_manifests,
                asset_folder: asset_folder.as_deref(),
            },
        );
    }

    /// Permissions, services, background shims and `Info.plist` entries
    /// of the plugins the app actually wires.
    fn emit_wired_surfaces(&self, android_package: Option<&str>, dep_manifests: &[Manifest]) {
        let mut wired_entries = resolve_wired_plugin_entries(&self.root, dep_manifests);
        if !self.config.android_permissions.is_empty() {
            wired_entries.push(PluginEntry {
                id: "app.android_permissions".to_owned(),
                android_permissions: self.config.android_permissions.clone(),
                ..PluginEntry::default()
            });
        }
        if self.android_active {
            emit_wired_android_permissions(&self.android_root, &wired_entries);
        }
        emit_service_and_background(
            &wired_entries,
            &ServiceEmitOpts {
                android_active: self.android_active,
                android_root: &self.android_root,
                android_package,
                ios_active: self.ios_active,
                ios_root: &self.ios_root,
                ios_app_dir: self.ios_app_dir.as_deref(),
                ios_plugins_subdir: &self.ios_plugins_subdir,
                ios_info_plist_overrides: &self.config.info_plist,
                document_types: &self.config.document_types,
                lib_name: &self.app.krate.lib_name,
            },
        );
    }
}

/// Crate files every app-side run reads. Tracked before the early
/// returns so plugin and desktop-only crates rerun on them too; the
/// `src/` entry points feed wired-plugin extraction.
fn track_crate_inputs(root: &Path) {
    crate::directive::rerun_if_changed(Path::new("build.rs"));
    for file in ["istmo.toml", "Cargo.toml", "src/lib.rs", "src/main.rs"] {
        crate::directive::rerun_if_exists(&root.join(file));
    }
}

/// Android project files read by namespace detection, the doctor and
/// the `AndroidManifest.xml` marker patches.
fn track_android_inputs(android_root: &Path) {
    for file in [
        "app/build.gradle.kts",
        "app/build.gradle",
        "app/src/main/AndroidManifest.xml",
    ] {
        crate::directive::rerun_if_exists(&android_root.join(file));
    }
}

/// iOS project files read by app-dir detection, the doctor and the
/// `Info.plist` marker patches.
///
/// Detection depends on the listing of `ios/`, which Cargo can only
/// watch as the whole directory, recursively. Once an app dir is found
/// that watch would rerun the build script (and recompile the crate)
/// whenever Xcode touches its user state or the build writes into
/// `ios/`, so it is kept only while detection finds nothing; a found
/// dir that goes away still reruns through its `Info.plist`.
fn track_ios_inputs(ios_root: &Path, app_dir: Option<&str>, app_dir_detected: bool) {
    if app_dir_detected && app_dir.is_none() {
        crate::directive::rerun_if_changed(ios_root);
    }
    crate::directive::rerun_if_exists(&ios_root.join("project.yml"));
    if let Some(app_dir) = app_dir {
        crate::directive::rerun_if_exists(&ios_root.join(app_dir).join("Info.plist"));
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

/// Write `android/.istmo/` for the `io.github.sergioribera.istmo` Gradle plugin.
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
    report_icon(project.write_icon(icon, &app.icon));
}

/// Crate error policy for icon output: an unreadable or undecodable
/// source is bad user configuration and fails the build; a failed write
/// into a generated directory only warns. Returns whether it succeeded.
fn report_icon(result: Result<(), IconError>) -> bool {
    match result {
        Ok(()) => true,
        Err(err @ IconError::Write { .. }) => {
            crate::directive::warning(&err.to_string());
            false
        }
        Err(err) => panic!("istmo-build: {err}"),
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
    asset_folder: Option<&'a Path>,
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
        report_icon(project.write_icon(icon));
    }
    let plugin_min_ios = sync
        .dep_manifests
        .iter()
        .filter_map(|m| m.min_versions.ios.map(|v| (v, m.primary_id().to_owned())))
        .max_by_key(|(v, _)| *v);
    project.write_xcodegen_fragment(
        sync.app,
        sync.native_dirs,
        plugin_min_ios.as_ref(),
        sync.asset_folder,
    );
    emit_ios_plist_fragments(project.root(), project.app_dir(), sync.native_dirs);
}

/// Delete a file istmo-build generated earlier and no longer emits.
/// Files without the generated header are left alone.
fn remove_generated(path: &Path) {
    if fs::read_to_string(path).is_ok_and(|text| text.contains("GENERATED by istmo-build")) {
        let _ = fs::remove_file(path);
    }
}

pub(crate) fn cargo_manifest_dir() -> PathBuf {
    PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR")
            .expect("CARGO_MANIFEST_DIR must be set (build.rs only)"),
    )
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

pub(crate) use crate::fsx::write_generated as write_if_changed;

/// Desktop targets: stage the window icon PNG behind
/// `ISTMO_APP_ICON_PNG` (read by `istmo::app_icon!()`), and on Windows
/// link the `.exe` icon and the composed application manifest.
fn emit_desktop_icon(
    app: &AppMetadata,
    dep_manifests_by_links: &[(String, Manifest)],
    app_manifest: Option<&Manifest>,
) {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    if matches!(target_os.as_str(), "android" | "ios") {
        return;
    }
    let Some(out_dir) = std::env::var_os("OUT_DIR").map(PathBuf::from) else {
        return;
    };
    let icon = app.icon.desktop_source().map(|path| {
        AppIcon::open(path).unwrap_or_else(|err| panic!("istmo-build: `[app.icon] desktop`: {err}"))
    });
    if let Some(icon) = &icon {
        let png = out_dir.join(DESKTOP_ICON_PNG);
        // Without the PNG `istmo::app_icon!()` falls back to `None`.
        if report_icon(icon.write_png(&png, DESKTOP_ICON_EDGE)) {
            println!("cargo::rustc-env=ISTMO_APP_ICON_PNG={}", png.display());
        }
    }
    if target_os != "windows" {
        return;
    }
    let fragments = dep_manifests_by_links
        .iter()
        .map(|(_, manifest)| manifest)
        .chain(app_manifest)
        .flat_map(|manifest| &manifest.windows_manifest_fragments);
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    WindowsResources {
        icon: icon.as_ref(),
        manifest: compose_manifest(fragments),
    }
    .emit(&out_dir, WindowsToolchain::from(target_env.as_str()))
    .unwrap_or_else(|err| panic!("istmo-build: {err}"));
}

/// Bake runtime environment variables used by `istmo::path` and the
/// desktop `istmo::assets` backend at compile time.
///
/// Emits:
/// - `ISTMO_APP_BUNDLE_ID` — sourced from `[app] id` in `istmo.toml`
///   with a `CARGO_PKG_NAME` fallback. `istmo::path` reads this to
///   derive per-app subdirectories on Linux, macOS and Windows.
/// - `ISTMO_APP_ASSETS_DIR` — set only when `[app] assets_dir` is
///   present. Relative paths resolve against the crate root; absolute
///   paths pass through verbatim. Used as tier 2 in the desktop asset
///   resolution chain (after the runtime `ISTMO_ASSETS_DIR` override,
///   before the `<exe_dir>/assets/` scan).
fn emit_runtime_env(
    app: &AppMetadata,
    config: &AppConfig,
    root: &Path,
    desktop_staging: Option<&Path>,
) {
    let bundle_id = app
        .id
        .as_ref()
        .map_or_else(|| app.krate.package.clone(), |id| id.as_str().to_owned());
    println!("cargo::rustc-env=ISTMO_APP_BUNDLE_ID={bundle_id}");
    println!(
        "cargo::rustc-env=ISTMO_SINGLE_INSTANCE={}",
        u8::from(config.single_instance())
    );
    // Priority: `[app] assets_dir` beats the auto-staged OUT_DIR
    // directory. The runtime `ISTMO_ASSETS_DIR` env var still overrides
    // both at read time.
    let assets_dir = config
        .assets_dir
        .as_ref()
        .map(|dir| {
            if dir.is_absolute() {
                dir.clone()
            } else {
                root.join(dir)
            }
        })
        .or_else(|| desktop_staging.map(Path::to_path_buf));
    if let Some(dir) = assets_dir {
        println!("cargo::rustc-env=ISTMO_APP_ASSETS_DIR={}", dir.display());
    }
}

/// `handed_over` plus `extra`, where an extra contract replaces any
/// earlier one with its plugin id, so each id generates one set of
/// sources.
fn with_extra_contracts(mut handed_over: Vec<Contract>, extra: &[Contract]) -> Vec<Contract> {
    for contract in extra {
        if let Some(slot) = handed_over
            .iter_mut()
            .find(|c| c.plugin_id == contract.plugin_id)
        {
            println!(
                "cargo::warning=istmo: an extra contract replaces the earlier contract of plugin `{}`",
                contract.plugin_id,
            );
            *slot = contract.clone();
        } else {
            handed_over.push(contract.clone());
        }
    }
    handed_over
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named_contract(plugin_id: &str, type_name: &str) -> Contract {
        Contract {
            plugin_id: plugin_id.to_owned(),
            type_name: type_name.to_owned(),
            methods: vec![],
            init: None,
            types: vec![],
        }
    }

    #[test]
    fn an_extra_contract_replaces_the_handed_over_one_with_its_id() {
        let merged = with_extra_contracts(
            vec![named_contract("a", "A"), named_contract("b", "B")],
            &[named_contract("b", "Override"), named_contract("c", "C")],
        );
        let names: Vec<(&str, &str)> = merged
            .iter()
            .map(|c| (c.plugin_id.as_str(), c.type_name.as_str()))
            .collect();
        assert_eq!(names, [("a", "A"), ("b", "Override"), ("c", "C")]);
    }

    fn write(path: &Path, contents: &str) {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }

    #[test]
    fn every_read_input_gets_a_rerun_line() {
        let root = std::env::temp_dir().join(format!("istmo-rerun-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("istmo.toml"), "[app]\nname = \"Demo\"\n");
        write(&root.join("Cargo.toml"), "[package]\nname = \"demo\"\n");
        write(&root.join("src/lib.rs"), "istmo::runtime!(plugins: []);\n");
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let gradle = root.join("android/app/build.gradle.kts");
        write(
            &gradle,
            "android {\n    namespace = \"com.example.demo\"\n}\n",
        );
        let android_manifest = root.join("android/app/src/main/AndroidManifest.xml");
        write(&android_manifest, "<manifest>\n</manifest>\n");
        let info_plist = root.join("ios/Demo/Info.plist");
        write(&info_plist, "<plist><dict></dict></plist>\n");
        write(&root.join("ios/project.yml"), "name: Demo\n");

        let ((), lines) = crate::directive::capture(|| {
            emit_app_with(AppOpts {
                root: Some(root.clone()),
                lib_name: Some("demo".into()),
                ..AppOpts::default()
            });
        });
        let rerun: Vec<&str> = lines
            .iter()
            .filter_map(|line| line.strip_prefix("cargo:rerun-if-changed="))
            .collect();
        let expected = [
            PathBuf::from("build.rs"),
            root.join("istmo.toml"),
            root.join("Cargo.toml"),
            root.join("src/lib.rs"),
            root.join("src/main.rs"),
            gradle,
            android_manifest,
            root.join("ios/project.yml"),
            info_plist,
        ];
        for path in &expected {
            let path = path.display().to_string();
            assert!(
                rerun.contains(&path.as_str()),
                "missing {path} in {rerun:#?}"
            );
        }
        // Only inputs that exist are tracked: Cargo reruns forever on a
        // missing one.
        let groovy = root.join("android/app/build.gradle").display().to_string();
        assert!(!rerun.contains(&groovy.as_str()));
        // A detected app dir stops the recursive `ios/` watch.
        let ios = root.join("ios").display().to_string();
        assert!(!rerun.contains(&ios.as_str()), "{rerun:#?}");
    }

    #[test]
    fn undetectable_ios_app_dir_watches_the_ios_directory() {
        let root =
            std::env::temp_dir().join(format!("istmo-rerun-ios-none-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("ios")).unwrap();
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let ((), lines) = crate::directive::capture(|| {
            emit_app_with(AppOpts {
                root: Some(root.clone()),
                android: Some(false),
                lib_name: Some("demo".into()),
                ..AppOpts::default()
            });
        });
        let ios = format!("cargo:rerun-if-changed={}", root.join("ios").display());
        assert!(lines.contains(&ios), "{lines:#?}");
    }

    #[test]
    fn configured_ios_app_dir_does_not_watch_the_ios_directory() {
        let root = std::env::temp_dir().join(format!("istmo-rerun-ios-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        write(&root.join("ios/Demo/Info.plist"), "<plist/>\n");
        write(&root.join("src/main.rs"), "fn main() {}\n");
        let ((), lines) = crate::directive::capture(|| {
            emit_app_with(AppOpts {
                root: Some(root.clone()),
                android: Some(false),
                ios_app_dir: Some("Demo".into()),
                lib_name: Some("demo".into()),
                ..AppOpts::default()
            });
        });
        let ios = format!("cargo:rerun-if-changed={}", root.join("ios").display());
        assert!(!lines.contains(&ios), "{lines:#?}");
    }
}
