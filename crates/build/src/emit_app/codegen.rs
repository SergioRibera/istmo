//! Kotlin / Swift sources per plugin contract, the plugin registries
//! and the optional seeded backend skeletons.

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use super::{remove_generated, write_if_changed};
use crate::app_config::Role;
use crate::codegen::{kt_string, swift_string};
use crate::contract::Contract;
use crate::kotlin_client::generate_kotlin_client;
use crate::kotlin_host::{generate_kotlin_codecs_interface, generate_kotlin_host};
use crate::kotlin_types::{generate_kotlin_codecs, generate_kotlin_types};
use crate::plugin_registry::{
    KOTLIN_REGISTRY_PACKAGE, RegistryEntry, generate_kotlin_plugin_registry,
};
use crate::swift::generate_swift_client;
use crate::swift_host::generate_swift_host;
use crate::swift_types::{generate_swift_codecs, generate_swift_types};
pub(super) fn emit_kotlin_registry(
    android_root: &Path,
    codegen_package: &str,
    entries: &[RegistryEntry],
) {
    let java_root = android_root.join("app/src/main/java");
    let dest = java_root
        .join(KOTLIN_REGISTRY_PACKAGE.replace('.', "/"))
        .join("IstmoPluginRegistry.kt");
    write_if_changed(
        &dest,
        &generate_kotlin_plugin_registry(codegen_package, entries),
    );
    if let Some(dir) = dest.parent() {
        mark_codegen_dir(dir);
    }
    // Before 0.2 the registry lived in the codegen package.
    let legacy = java_root
        .join(codegen_package.replace('.', "/"))
        .join("IstmoPluginRegistry.kt");
    if legacy != dest {
        remove_generated(&legacy);
    }
}

pub(super) fn emit_kotlin(
    java_root: &Path,
    package: &str,
    contract: &Contract,
    role: Role,
    seed_backends: bool,
) {
    let full_package = package.to_owned();
    let pkg_path = full_package.replace('.', "/");
    let gen_dir = java_root.join(pkg_path);
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

pub(super) fn emit_swift(
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
    mark_codegen_dir(&generated);
}

/// [`crate::fsx::mark_generated_dir`] for a codegen output directory,
/// unless it holds a seeded `*BackendImpl` the user now owns; then a
/// marker left by an earlier build is dropped so that file is not
/// ignored.
pub(super) fn mark_codegen_dir(dir: &Path) {
    let holds_user_code = fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.contains("BackendImpl."))
        })
    });
    if holds_user_code {
        crate::fsx::unmark_generated_dir(dir);
    } else {
        crate::fsx::mark_generated_dir(dir);
    }
}

/// Runtime types generated Kotlin may name unqualified.
const KOTLIN_RUNTIME_SYMBOLS: [&str; 8] = [
    "BackendException",
    "Bincode",
    "HandleReleaser",
    "IstmoRuntime",
    "NativeHandleId",
    "PluginException",
    "PluginHandler",
    "PluginResult",
];

/// `body` under `package`, importing exactly the runtime types it names
/// (an unused import is a warning in every Kotlin toolchain).
fn kotlin_with_package(package: &str, body: &str) -> String {
    const RUNTIME_PACKAGE: &str = "dev.istmo.runtime";
    if package == RUNTIME_PACKAGE {
        return format!("package {package}\n\n{body}");
    }
    let mut imports = String::new();
    for symbol in KOTLIN_RUNTIME_SYMBOLS {
        if kotlin_code_names(body, symbol) {
            let _ = writeln!(imports, "import {RUNTIME_PACKAGE}.{symbol}");
        }
    }
    if imports.is_empty() {
        return format!("package {package}\n\n{body}");
    }
    format!("package {package}\n\n{imports}\n{body}")
}

/// Whether a non-comment line of `body` uses `ident` as a whole word.
fn kotlin_code_names(body: &str, ident: &str) -> bool {
    let is_ident_char = |c: char| c.is_ascii_alphanumeric() || c == '_';
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| {
            line.match_indices(ident).any(|(at, _)| {
                let before = line[..at].chars().next_back();
                let after = line[at + ident.len()..].chars().next();
                !before.is_some_and(is_ident_char) && !after.is_some_and(is_ident_char)
            })
        })
}

fn write_if_absent(dest: &Path, contents: &str) {
    if dest.exists() {
        return;
    }
    if let Err(err) = crate::fsx::write_if_changed(dest, contents.as_bytes()) {
        println!(
            "cargo::warning=istmo-build: failed to seed {}: {err}",
            dest.display()
        );
    }
}

/// Seeds `<T>BackendImpl.kt` (plus `<T>FactoryImpl` for a stateful
/// plugin) with the constructor the generated plugin registry calls and
/// one `TODO()` override per backend method.
fn seed_kotlin_backend_impl(gen_dir: &Path, package: &str, contract: &Contract) {
    let ty = &contract.type_name;
    let mut body = String::from(
        "// Seeded by istmo-build. Fill in each `TODO()` with your logic;\n\
         // regenerate is a no-op once this file exists.\n\n\
         import android.content.Context\n\n",
    );
    if let Some(init) = &contract.init {
        let config = init.to_kotlin();
        let _ = write!(
            body,
            "class {ty}FactoryImpl(\n    private val context: Context,\n) : {ty}Factory {{\n\
             \x20   override suspend fun create(config: {config}): {ty}Backend =\n\
             \x20       {ty}BackendImpl(context, config)\n}}\n\n\
             class {ty}BackendImpl(\n    private val context: Context,\n\
             \x20   private val config: {config},\n) : {ty}Backend {{\n"
        );
    } else {
        let _ = write!(
            body,
            "class {ty}BackendImpl(\n    private val context: Context,\n) : {ty}Backend {{\n"
        );
    }
    for (i, method) in contract.methods.iter().enumerate() {
        if i > 0 {
            body.push('\n');
        }
        let _ = write!(
            body,
            "    override {} {{\n        TODO({})\n    }}\n",
            crate::kotlin_host::backend_method_decl(method),
            kt_string(&format!("implement {}", method.name)),
        );
    }
    body.push_str("}\n");
    write_if_absent(
        &gen_dir.join(format!("{ty}BackendImpl.kt")),
        &kotlin_with_package(package, &body),
    );
}

/// Seeds `<T>BackendImpl.swift` (plus `<T>FactoryImpl` for a stateful
/// plugin) with the initializer the generated plugin registry calls and
/// one `fatalError` body per backend requirement.
fn seed_swift_backend_impl(gen_dir: &Path, contract: &Contract) {
    let ty = &contract.type_name;
    let mut body = String::from(
        "import Foundation\nimport IstmoRuntime\n\n\
         // Seeded by istmo-build. Fill in each `fatalError` with your\n\
         // logic; regenerate is a no-op once this file exists.\n\n",
    );
    if let Some(init) = &contract.init {
        let config = init.to_swift();
        let _ = write!(
            body,
            "final class {ty}FactoryImpl: {ty}Factory {{\n\
             \x20   func create(config: {config}) async throws -> {ty}Backend {{\n\
             \x20       {ty}BackendImpl(config: config)\n    }}\n}}\n\n\
             final class {ty}BackendImpl: {ty}Backend {{\n\
             \x20   private let config: {config}\n\n\
             \x20   init(config: {config}) {{\n        self.config = config\n    }}\n"
        );
        if !contract.methods.is_empty() {
            body.push('\n');
        }
    } else {
        let _ = writeln!(body, "final class {ty}BackendImpl: {ty}Backend {{");
    }
    for (i, method) in contract.methods.iter().enumerate() {
        if i > 0 {
            body.push('\n');
        }
        let _ = write!(
            body,
            "    {} {{\n        fatalError({})\n    }}\n",
            crate::swift_host::backend_method_decl(method),
            swift_string(&format!("implement {}", method.name)),
        );
    }
    body.push_str("}\n");
    write_if_absent(&gen_dir.join(format!("{ty}BackendImpl.swift")), &body);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_backends_escape_keyword_names() {
        use crate::contract::{Arg, Method, MethodKind, TypeRef};
        let dir = std::env::temp_dir().join(format!("istmo-seed-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let contract = Contract {
            plugin_id: "istmo.k".into(),
            type_name: "K".into(),
            methods: vec![Method {
                name: "in".into(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "is".into(),
                    ty: TypeRef::Bool,
                }],
                returns: TypeRef::Unit,
                error: None,
            }],
            init: None,
            types: Vec::new(),
        };
        seed_kotlin_backend_impl(&dir, "com.example", &contract);
        seed_swift_backend_impl(&dir, &contract);
        let kotlin = fs::read_to_string(dir.join("KBackendImpl.kt")).unwrap();
        assert!(
            kotlin.contains("override suspend fun `in`(`is`: Boolean) {"),
            "{kotlin}"
        );
        let swift = fs::read_to_string(dir.join("KBackendImpl.swift")).unwrap();
        assert!(
            swift.contains("func `in`(`is`: Bool) async throws -> Void {"),
            "{swift}"
        );
    }

    #[test]
    fn kotlin_imports_only_the_runtime_types_the_code_names() {
        let body = "// Returns a PluginResult.\n\
                    fun readNativeHandleId(b: ByteArray): Bincode.Decoded<Long> = TODO()\n";
        let src = kotlin_with_package("com.example.gen", body);
        assert!(src.contains("import dev.istmo.runtime.Bincode\n"), "{src}");
        assert!(!src.contains("PluginResult\n"), "{src}");
        assert!(
            !src.contains("import dev.istmo.runtime.NativeHandleId"),
            "{src}"
        );
        let bare = kotlin_with_package("com.example.gen", "class A\n");
        assert_eq!(bare, "package com.example.gen\n\nclass A\n");
        let runtime = kotlin_with_package("dev.istmo.runtime", body);
        assert!(!runtime.contains("import"), "{runtime}");
    }

    #[test]
    fn seeding_a_backend_into_a_marked_dir_unhides_it() {
        let dir = std::env::temp_dir().join(format!("istmo-seed-mark-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        mark_codegen_dir(&dir);
        assert!(dir.join(".gitignore").exists());
        fs::write(dir.join("KBackendImpl.swift"), "// mine\n").unwrap();
        mark_codegen_dir(&dir);
        assert!(!dir.join(".gitignore").exists());
    }
}
