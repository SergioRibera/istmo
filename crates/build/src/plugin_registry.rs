//! Generators for `IstmoPluginRegistry` — a single Kotlin `object` /
//! Swift `enum` that registers every eligible plugin dispatcher with
//! the runtime in one call — and for the Swift `IstmoApp.run()` entry.
//!
//! The Kotlin registry always lives at
//! [`KOTLIN_REGISTRY_CLASS`] so `dev.istmo.runtime.IstmoHost` can
//! find it without configuration, the way Flutter's embedding finds
//! `GeneratedPluginRegistrant`.
//!
//! Emitted only for plugins that have `auto_register = true` in their
//! `istmo.toml` (default) and are not overridden `auto_register = false`
//! at the app level. A plugin whose Android backend needs the host
//! activity declares `android_backend_arg = "activity"` (or the
//! concrete `Activity` subclass) instead of opting out.

use std::fmt::Write as _;

use crate::codegen::kt_string;
use crate::contract::Contract;
use crate::manifest::{AndroidBackendArg, PluginEntry};

/// Package of the generated Kotlin registry.
pub const KOTLIN_REGISTRY_PACKAGE: &str = "dev.istmo.generated";

/// Fully-qualified name of the generated Kotlin registry.
pub const KOTLIN_REGISTRY_CLASS: &str = "dev.istmo.generated.IstmoPluginRegistry";

/// One plugin's participation in the auto-registration codegen.
#[derive(Debug, Clone)]
pub struct RegistryEntry {
    pub plugin_id: String,
    pub type_name: String,
    /// Stateful plugins (`init: Config`) require a `<T>FactoryImpl`
    /// implementing the generated `<T>Factory` interface. Stateless
    /// plugins register their `<T>BackendImpl` directly.
    pub stateful: bool,
    /// Package the plugin's dispatcher and codecs were generated into
    /// (`[plugin] android_package`); `None` means the app's codegen
    /// package.
    pub android_package: Option<String>,
    /// Fully-qualified Kotlin class passed to the dispatcher; `None`
    /// means `<T>BackendImpl` / `<T>FactoryImpl` in the plugin's package.
    pub android_class: Option<String>,
    pub android_arg: AndroidBackendArg,
    /// Swift type passed to the dispatcher; `None` means
    /// `<T>BackendImpl` / `<T>FactoryImpl`.
    pub ios_class: Option<String>,
}

impl RegistryEntry {
    #[must_use]
    pub fn from_contract(contract: &Contract) -> Self {
        Self {
            plugin_id: contract.plugin_id.clone(),
            type_name: contract.type_name.clone(),
            stateful: contract.init.is_some(),
            android_package: None,
            android_class: None,
            android_arg: AndroidBackendArg::Context,
            ios_class: None,
        }
    }

    /// Apply the backend overrides the plugin declares in its
    /// `istmo.toml`.
    #[must_use]
    pub fn with_plugin(mut self, plugin: &PluginEntry) -> Self {
        self.android_package.clone_from(&plugin.android_package);
        self.android_class.clone_from(&plugin.android_backend);
        self.android_arg = plugin.android_backend_arg.clone();
        self.ios_class.clone_from(&plugin.ios_backend);
        self
    }

    fn default_class(&self) -> String {
        let suffix = if self.stateful {
            "FactoryImpl"
        } else {
            "BackendImpl"
        };
        format!("{}{suffix}", self.type_name)
    }

    const fn is_activity_bound(&self) -> bool {
        matches!(self.android_arg, AndroidBackendArg::Activity(_))
    }

    /// Package holding the generated dispatcher and codecs.
    fn package<'a>(&'a self, codegen_package: &'a str) -> &'a str {
        self.android_package.as_deref().unwrap_or(codegen_package)
    }

    fn backend_class(&self, codegen_package: &str) -> String {
        self.android_class.clone().unwrap_or_else(|| {
            format!("{}.{}", self.package(codegen_package), self.default_class())
        })
    }

    /// Name of the registry field caching an activity-bound backend.
    fn backend_field(&self) -> String {
        let mut chars = self.type_name.chars();
        chars.next().map_or_else(String::new, |first| {
            format!("{}{}Backend", first.to_lowercase(), chars.as_str())
        })
    }

    /// `IstmoRuntime.registerHandler(...)` for this plugin at `indent`;
    /// `backend` is the Kotlin expression for the backend instance.
    fn write_register(&self, out: &mut String, codegen_package: &str, indent: &str, backend: &str) {
        let ty = &self.type_name;
        let package = self.package(codegen_package);
        writeln!(out, "{indent}IstmoRuntime.registerHandler(").ok();
        writeln!(out, "{indent}    {package}.{ty}Dispatcher.PLUGIN_ID,").ok();
        writeln!(out, "{indent}    {package}.{ty}Dispatcher(").ok();
        writeln!(out, "{indent}        {backend},").ok();
        writeln!(out, "{indent}        {package}.{ty}CodecsImpl(),").ok();
        writeln!(out, "{indent}    ),").ok();
        writeln!(out, "{indent})").ok();
    }

    /// Hand `activity` to the cached backend when it is
    /// `IstmoActivityBound`, otherwise build and register a new one.
    fn write_kotlin_activity(&self, out: &mut String, codegen_package: &str, class: &str) {
        let field = self.backend_field();
        let backend = self.backend_class(codegen_package);
        writeln!(
            out,
            "        hostActivity<{class}>(activity, {}).let {{ host ->",
            kt_string(&self.plugin_id)
        )
        .ok();
        writeln!(out, "            val current = {field}").ok();
        out.push_str("            if (current is IstmoActivityBound) {\n");
        out.push_str("                current.attachActivity(host)\n");
        out.push_str("            } else {\n");
        writeln!(out, "                val backend = {backend}(host)").ok();
        writeln!(out, "                {field} = backend").ok();
        self.write_register(out, codegen_package, "                ", "backend");
        out.push_str("            }\n");
        out.push_str("        }\n");
    }

    /// Drop a backend that would keep its activity alive; an
    /// `IstmoActivityBound` one holds it weakly and stays registered.
    fn write_kotlin_release(&self, out: &mut String, codegen_package: &str) {
        let field = self.backend_field();
        writeln!(out, "        if ({field} !is IstmoActivityBound) {{").ok();
        writeln!(
            out,
            "            IstmoRuntime.unregisterHandler({}.{}Dispatcher.PLUGIN_ID)",
            self.package(codegen_package),
            self.type_name
        )
        .ok();
        writeln!(out, "            {field} = null").ok();
        out.push_str("        }\n");
    }
}

/// Emit `IstmoPluginRegistry.kt` in [`KOTLIN_REGISTRY_PACKAGE`].
///
/// `codegen_package` is the package the per-plugin dispatchers, codecs
/// and default backends were generated into (`[app] android_package`)
/// for plugins that do not declare their own `[plugin] android_package`.
///
/// Handlers live in the process-wide `IstmoRuntime`, so context-bound
/// backends get the application context and never pin an activity.
/// Activity-bound backends go through `registerActivityPlugins`, which
/// `IstmoHost` calls for every new activity: a backend implementing
/// `IstmoActivityBound` is built once and handed each activity, any
/// other is rebuilt. `releaseActivityPlugins` unregisters the latter
/// once no activity is left.
#[must_use]
pub fn generate_kotlin_plugin_registry(codegen_package: &str, entries: &[RegistryEntry]) -> String {
    let (activity_bound, context_bound): (Vec<_>, Vec<_>) =
        entries.iter().partition(|e| e.is_activity_bound());
    let needs_activity = !activity_bound.is_empty();
    let mut out = String::new();
    writeln!(&mut out, "package {KOTLIN_REGISTRY_PACKAGE}\n").ok();
    out.push_str("// GENERATED by istmo-build - DO NOT EDIT.\n");
    out.push_str("// Registers every auto-registered plugin dispatcher with the istmo\n");
    out.push_str("// runtime. `IstmoHost` (and so `IstmoGameActivity` /\n");
    out.push_str("// `IstmoNativeActivity`) calls it before the Rust entry point runs.\n");
    out.push_str("// Plugins with bespoke construction opt out via `auto_register = false`\n");
    out.push_str("// in `istmo.toml`.\n\n");
    if needs_activity {
        out.push_str("import android.app.Activity\n");
    }
    out.push_str("import android.content.Context\n");
    if needs_activity {
        out.push_str("import android.util.Log\n");
        out.push_str("import dev.istmo.runtime.IstmoActivityBound\n");
    }
    out.push_str("import dev.istmo.runtime.IstmoPluginRegistrant\n");
    out.push_str("import dev.istmo.runtime.IstmoRuntime\n\n");
    out.push_str("object IstmoPluginRegistry : IstmoPluginRegistrant {\n");
    for entry in &activity_bound {
        writeln!(
            &mut out,
            "    private var {}: Any? = null",
            entry.backend_field()
        )
        .ok();
    }
    if needs_activity {
        out.push('\n');
    }
    out.push_str("    override fun registerAll(context: Context) {\n");
    if entries.is_empty() {
        out.push_str("        // No auto-registerable plugins in this build.\n");
    }
    if !context_bound.is_empty() {
        out.push_str("        val appContext = context.applicationContext ?: context\n");
    }
    for entry in &context_bound {
        let backend = format!("{}(appContext)", entry.backend_class(codegen_package));
        entry.write_register(&mut out, codegen_package, "        ", &backend);
    }
    if needs_activity {
        out.push_str(KOTLIN_REGISTER_ACTIVITY_PLUGINS_CALL);
    }
    out.push_str("    }\n");
    if needs_activity {
        out.push_str("\n    override fun registerActivityPlugins(activity: Activity) {\n");
        for entry in &activity_bound {
            if let AndroidBackendArg::Activity(class) = &entry.android_arg {
                entry.write_kotlin_activity(&mut out, codegen_package, class);
            }
        }
        out.push_str("    }\n");
        out.push_str("\n    override fun releaseActivityPlugins() {\n");
        for entry in &activity_bound {
            entry.write_kotlin_release(&mut out, codegen_package);
        }
        out.push_str("    }\n");
        out.push_str(KOTLIN_HOST_ACTIVITY_HELPER);
    }
    out.push_str("}\n");
    out
}

const KOTLIN_REGISTER_ACTIVITY_PLUGINS_CALL: &str = r#"        if (context is Activity) {
            registerActivityPlugins(context)
        } else {
            Log.w(TAG, "activity-bound plugins skipped when registering from ${context.javaClass.name}")
        }
"#;

const KOTLIN_HOST_ACTIVITY_HELPER: &str = r#"
    private inline fun <reified A : Activity> hostActivity(activity: Activity, pluginId: String): A =
        activity as? A ?: throw IllegalStateException(
            "istmo plugin $pluginId needs its host activity to extend ${A::class.java.name}, " +
                "but ${activity.javaClass.name} does not",
        )

    private const val TAG = "istmo"
"#;

/// Emit `IstmoPluginRegistry.swift`.
#[must_use]
pub fn generate_swift_plugin_registry(entries: &[RegistryEntry]) -> String {
    let mut out = String::new();
    out.push_str("// GENERATED by istmo-build - DO NOT EDIT.\n");
    out.push_str("// Registers every auto-registered plugin dispatcher with the istmo\n");
    out.push_str("// runtime. Plugins with bespoke construction opt out via\n");
    out.push_str("// `auto_register = false` in `istmo.toml`.\n\n");
    out.push_str("import Foundation\n");
    out.push_str("import IstmoRuntime\n\n");
    out.push_str("enum IstmoPluginRegistry {\n");
    out.push_str("    static func registerAll(runtime: IstmoRuntime = .shared) {\n");
    if entries.is_empty() {
        out.push_str("        // No auto-registerable plugins in this build.\n");
    }
    for entry in entries {
        let ty = &entry.type_name;
        let label = if entry.stateful { "factory" } else { "backend" };
        let class = entry
            .ios_class
            .clone()
            .unwrap_or_else(|| entry.default_class());
        writeln!(&mut out, "        runtime.registerHandler(").ok();
        writeln!(&mut out, "            {ty}Dispatcher.PLUGIN_ID,").ok();
        writeln!(
            &mut out,
            "            {ty}Dispatcher({label}: {class}(), codecs: {ty}CodecsImpl())"
        )
        .ok();
        writeln!(&mut out, "        )").ok();
    }
    out.push_str("    }\n");
    out.push_str("}\n");
    out
}

/// Emit `IstmoMain.swift`, the `IstmoApp.run()` entry for Rust-driven apps.
///
/// It starts the runtime, registers every plugin and hands control to
/// the crate's `#[istmo::mobile_app]` entry point (`istmo_run_ios`), so
/// an app's `main.swift` is the single line `IstmoApp.run()`.
///
/// Only emitted for crates that export that entry point: the
/// `@_silgen_name` declaration is an undefined symbol otherwise.
#[must_use]
pub fn generate_swift_app_entry() -> String {
    "// GENERATED by istmo-build - DO NOT EDIT.\n\
     import IstmoRuntime\n\
     \n\
     @_silgen_name(\"istmo_run_ios\")\n\
     private func istmo_run_ios() -> Int32\n\
     \n\
     extension IstmoApp {\n\
     \x20   /// Start the runtime, register every auto-registered plugin and run\n\
     \x20   /// the crate's `#[istmo::mobile_app]` entry point.\n\
     \x20   static func run() -> Never {\n\
     \x20       run(registerPlugins: { IstmoPluginRegistry.registerAll() }, entry: istmo_run_ios)\n\
     \x20   }\n\
     }\n"
    .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Contract, TypeDef};

    fn stateless(plugin_id: &str, type_name: &str) -> Contract {
        Contract {
            plugin_id: plugin_id.to_owned(),
            type_name: type_name.to_owned(),
            methods: vec![],
            init: None,
            types: vec![] as Vec<TypeDef>,
        }
    }

    fn entry(type_name: &str, stateful: bool) -> RegistryEntry {
        RegistryEntry {
            plugin_id: format!("acme.{}", type_name.to_lowercase()),
            type_name: type_name.into(),
            stateful,
            android_package: None,
            android_class: None,
            android_arg: AndroidBackendArg::Context,
            ios_class: None,
        }
    }

    #[test]
    fn stateless_contract_becomes_entry() {
        let c = stateless("acme.foo", "Foo");
        let entry = RegistryEntry::from_contract(&c);
        assert_eq!(entry.plugin_id, "acme.foo");
        assert_eq!(entry.type_name, "Foo");
        assert!(!entry.stateful);
        assert_eq!(entry.android_arg, AndroidBackendArg::Context);
    }

    #[test]
    fn stateful_contract_records_stateful_flag() {
        let mut c = stateless("acme.bar", "Bar");
        c.init = Some(crate::contract::TypeRef::U32);
        let entry = RegistryEntry::from_contract(&c);
        assert!(entry.stateful);
    }

    #[test]
    fn kotlin_registry_lives_in_fixed_package() {
        let src = generate_kotlin_plugin_registry("com.example.gen", &[]);
        assert!(src.starts_with("package dev.istmo.generated\n"));
        assert!(src.contains("object IstmoPluginRegistry : IstmoPluginRegistrant"));
        assert!(src.contains("No auto-registerable"));
        assert!(!src.contains("hostActivity"));
    }

    #[test]
    fn kotlin_registry_uses_factory_for_stateful() {
        let src = generate_kotlin_plugin_registry("com.example.gen", &[entry("A", true)]);
        assert!(src.contains("com.example.gen.AFactoryImpl(appContext)"));
        assert!(!src.contains("ABackendImpl"));
    }

    #[test]
    fn kotlin_registry_iterates_entries() {
        let src = generate_kotlin_plugin_registry(
            "com.example.gen",
            &[entry("A", false), entry("B", false)],
        );
        assert!(src.contains("val appContext = context.applicationContext ?: context\n"));
        assert!(src.contains("com.example.gen.ADispatcher.PLUGIN_ID"));
        assert!(src.contains("com.example.gen.ABackendImpl(appContext)"));
        assert!(src.contains("com.example.gen.BCodecsImpl()"));
        assert!(!src.contains("IstmoActivityBound"));
        assert!(!src.contains("registerActivityPlugins"));
    }

    #[test]
    fn kotlin_registry_registers_activity_plugins_apart() {
        let mut share = entry("Share", false);
        share.android_class = Some("dev.istmo.plugins.share.ShareBackendImpl".into());
        share.android_arg =
            AndroidBackendArg::Activity("androidx.activity.ComponentActivity".into());
        let src = generate_kotlin_plugin_registry("dev.istmo.runtime", &[entry("A", false), share]);
        assert!(src.contains("    private var shareBackend: Any? = null\n"));
        assert!(src.contains("override fun registerActivityPlugins(activity: Activity) {"));
        assert!(src.contains(
            "        hostActivity<androidx.activity.ComponentActivity>(activity, \"acme.share\").let { host ->\n\
             \x20           val current = shareBackend\n\
             \x20           if (current is IstmoActivityBound) {\n\
             \x20               current.attachActivity(host)\n\
             \x20           } else {\n\
             \x20               val backend = dev.istmo.plugins.share.ShareBackendImpl(host)\n\
             \x20               shareBackend = backend\n"
        ));
        assert!(src.contains(
            "        if (shareBackend !is IstmoActivityBound) {\n\
             \x20           IstmoRuntime.unregisterHandler(dev.istmo.runtime.ShareDispatcher.PLUGIN_ID)\n\
             \x20           shareBackend = null\n"
        ));
        assert!(src.contains("import dev.istmo.runtime.IstmoActivityBound"));
        assert!(src.contains("registerActivityPlugins(context)"));
        assert!(src.contains("private inline fun <reified A : Activity> hostActivity"));
        assert!(src.contains("import android.app.Activity"));
        let register_all = &src[src.find("fun registerAll").unwrap()..];
        let register_all = &register_all[..register_all.find("\n    }\n").unwrap()];
        assert!(register_all.contains("ABackendImpl(appContext)"));
        assert!(!register_all.contains("ShareBackendImpl"));
    }

    #[test]
    fn kotlin_registry_uses_the_plugin_package() {
        let mut share = entry("Share", false);
        share.android_package = Some("dev.istmo.plugins.share".into());
        share.android_arg =
            AndroidBackendArg::Activity("androidx.activity.ComponentActivity".into());
        let src = generate_kotlin_plugin_registry("com.example.gen", &[entry("A", false), share]);
        assert!(src.contains("com.example.gen.ADispatcher.PLUGIN_ID"));
        assert!(src.contains("val backend = dev.istmo.plugins.share.ShareBackendImpl(host)"));
        assert!(src.contains("dev.istmo.plugins.share.ShareDispatcher(\n"));
        assert!(src.contains("dev.istmo.plugins.share.ShareCodecsImpl(),"));
        assert!(src.contains(
            "IstmoRuntime.unregisterHandler(dev.istmo.plugins.share.ShareDispatcher.PLUGIN_ID)"
        ));
        assert!(!src.contains("com.example.gen.Share"));
    }

    #[test]
    fn swift_registry_calls_register_handler() {
        let src = generate_swift_plugin_registry(&[entry("A", false)]);
        assert!(src.contains("enum IstmoPluginRegistry"));
        assert!(src.contains("runtime.registerHandler("));
        assert!(src.contains("ADispatcher.PLUGIN_ID,"));
        assert!(src.contains("ADispatcher(backend: ABackendImpl(), codecs: ACodecsImpl())"));
    }

    #[test]
    fn swift_registry_uses_factory_and_override() {
        let mut a = entry("A", true);
        a.ios_class = Some("CustomFactory".into());
        let src = generate_swift_plugin_registry(&[a]);
        assert!(src.contains("ADispatcher(factory: CustomFactory(), codecs: ACodecsImpl())"));
    }

    #[test]
    fn swift_app_entry_wraps_rust_entry_point() {
        let src = generate_swift_app_entry();
        assert!(src.contains("@_silgen_name(\"istmo_run_ios\")"));
        assert!(src.contains("    static func run() -> Never {\n"));
        assert!(src.contains("entry: istmo_run_ios"));
    }
}
