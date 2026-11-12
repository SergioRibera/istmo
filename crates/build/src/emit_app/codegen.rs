//! Kotlin / Swift sources per plugin contract, the plugin registries
//! and the optional seeded backend skeletons.

use std::fs;
use std::path::Path;

use super::{remove_generated, write_if_changed};
use crate::app_config::Role;
use crate::codegen::{kt_ident, swift_ident};
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
/// unless it holds a seeded `*BackendImpl` the user now owns.
pub(super) fn mark_codegen_dir(dir: &Path) {
    let holds_user_code = fs::read_dir(dir).is_ok_and(|entries| {
        entries.filter_map(Result::ok).any(|entry| {
            entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.contains("BackendImpl."))
        })
    });
    if !holds_user_code {
        crate::fsx::mark_generated_dir(dir);
    }
}

fn kotlin_with_package(package: &str, body: &str) -> String {
    const RUNTIME_PACKAGE: &str = "dev.istmo.runtime";
    if package == RUNTIME_PACKAGE {
        return format!("package {package}\n\n{body}");
    }
    let imports = "\
import dev.istmo.runtime.BackendException\n\
import dev.istmo.runtime.Bincode\n\
import dev.istmo.runtime.HandleReleaser\n\
import dev.istmo.runtime.IstmoRuntime\n\
import dev.istmo.runtime.PluginException\n\
import dev.istmo.runtime.PluginHandler\n\
import dev.istmo.runtime.PluginResult\n";
    format!("package {package}\n\n{imports}\n{body}")
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

fn seed_kotlin_backend_impl(gen_dir: &Path, package: &str, contract: &Contract) {
    use std::fmt::Write as _;
    let ty = &contract.type_name;
    let mut body = format!(
        "package {package}\n\n\
         // Seeded by istmo-build. Fill in each `TODO()` with your logic;\n\
         // regenerate is a no-op once this file exists.\n\n\
         class {ty}BackendImpl : {ty}Backend {{\n"
    );
    for method in &contract.methods {
        let args = method
            .args
            .iter()
            .map(|a| format!("{}: Any", kt_ident(&kotlin_method_name(&a.name))))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            body,
            "    override suspend fun {}({args}): Any {{\n        TODO(\"implement {}\")\n    }}\n",
            kt_ident(&kotlin_method_name(&method.name)),
            method.name,
        );
    }
    body.push_str("}\n");
    write_if_absent(&gen_dir.join(format!("{ty}BackendImpl.kt")), &body);
}

fn seed_swift_backend_impl(gen_dir: &Path, contract: &Contract) {
    use std::fmt::Write as _;
    let ty = &contract.type_name;
    let mut body = format!(
        "import Foundation\n\n\
         // Seeded by istmo-build. Fill in each `fatalError` with your\n\
         // logic; regenerate is a no-op once this file exists.\n\n\
         final class {ty}BackendImpl: {ty}Backend {{\n"
    );
    for method in &contract.methods {
        let args = method
            .args
            .iter()
            .map(|a| format!("{}: Any", swift_ident(&a.name)))
            .collect::<Vec<_>>()
            .join(", ");
        let _ = write!(
            body,
            "    func {}({args}) async throws -> Any {{\n        fatalError(\"implement {}\")\n    }}\n",
            swift_ident(&method.name),
            method.name,
        );
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
        assert!(kotlin.contains("fun `in`(`is`: Any): Any"), "{kotlin}");
        let swift = fs::read_to_string(dir.join("KBackendImpl.swift")).unwrap();
        assert!(swift.contains("func `in`(`is`: Any)"), "{swift}");
    }

    #[test]
    fn kotlin_method_name_snake_to_camel() {
        assert_eq!(kotlin_method_name("sign_in"), "signIn");
        assert_eq!(kotlin_method_name("open_url"), "openUrl");
        assert_eq!(kotlin_method_name("echo"), "echo");
    }
}
