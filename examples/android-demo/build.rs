//! Regenerates the Kotlin glue for the demo's Rust-hosted `Echo` trait
//! and Kotlin-hosted `Notifier` trait.
//!
//! Runs on every `cargo build`. Generated files land under
//! `android/app/src/main/java/dev/istmo/demo/gen/` and are gitignored.
//! Contracts for both traits are constructed inline here since they are
//! demo-specific (not part of `istmo-plugins-schema`).

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{
    Contract, Field, Method, MethodKind, StructDef, TypeDef, TypeRef, generate_kotlin_client,
    generate_kotlin_codecs, generate_kotlin_codecs_interface, generate_kotlin_host,
    generate_kotlin_types,
};

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let gen_dir = root
        .join("android/app/src/main/java/dev/istmo/demo/gen")
        .canonicalize()
        .unwrap_or_else(|_| root.join("android/app/src/main/java/dev/istmo/demo/gen"));

    // Echo — Rust hosts, Kotlin calls via generated EchoClient.
    emit_client_bundle(&gen_dir, "dev.istmo.demo.gen", &echo_contract());

    // Notifier — Kotlin hosts, Rust calls via generated NotifierDispatcher.
    // Reuse EchoError from Echo's Types file — do NOT re-declare it here,
    // just set `types: vec![]` on the notifier contract so the types
    // generator produces no output.
    emit_host_bundle_without_types(&gen_dir, "dev.istmo.demo.gen", &notifier_contract());

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/lib.rs");
}

fn emit_client_bundle(gen_dir: &Path, package: &str, contract: &Contract) {
    let name = contract.type_name.as_str();
    emit(
        &gen_dir.join(format!("{name}Types.kt")),
        &with_package(package, &generate_kotlin_types(contract)),
    );
    emit(
        &gen_dir.join(format!("{name}Codecs.kt")),
        &with_package(package, &generate_kotlin_codecs_interface(contract)),
    );
    emit(
        &gen_dir.join(format!("{name}CodecsImpl.kt")),
        &with_package(package, &generate_kotlin_codecs(contract)),
    );
    emit(
        &gen_dir.join(format!("{name}Client.kt")),
        &with_package_and_imports(package, &generate_kotlin_client(contract)),
    );
}

/// Host bundle without emitting `<T>Types.kt` or `<T>CodecsImpl.kt` —
/// used when the contract's named types are already declared by a
/// sibling contract in the same package and the caller hand-writes a
/// codecs adapter that delegates to the sibling's codec impl.
fn emit_host_bundle_without_types(gen_dir: &Path, package: &str, contract: &Contract) {
    let name = contract.type_name.as_str();
    emit(
        &gen_dir.join(format!("{name}Host.kt")),
        &with_package_and_imports(package, &generate_kotlin_host(contract)),
    );
}

fn echo_contract() -> Contract {
    let echo_error = TypeRef::Named("EchoError".to_owned());
    let string_method = |name: &str, arg: &str| Method {
        name: name.to_owned(),
        kind: MethodKind::Unary,
        args: vec![istmo_build::Arg {
            name: arg.to_owned(),
            ty: TypeRef::String,
        }],
        returns: TypeRef::String,
        error: Some(echo_error.clone()),
    };
    Contract {
        plugin_id: "dev.istmo.demo.echo".to_owned(),
        type_name: "Echo".to_owned(),
        methods: vec![
            string_method("echo", "text"),
            string_method("check_permission", "permission"),
            string_method("request_permission", "permission"),
            string_method("open_url", "url"),
            Method {
                name: "lifecycle_snapshot".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::String,
                error: Some(echo_error.clone()),
            },
            Method {
                name: "drain_deeplinks".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Vec(Box::new(TypeRef::String)),
                error: Some(echo_error.clone()),
            },
            Method {
                name: "spam_notify".to_owned(),
                kind: MethodKind::Unary,
                args: vec![istmo_build::Arg {
                    name: "count".to_owned(),
                    ty: TypeRef::U32,
                }],
                returns: TypeRef::U32,
                error: Some(echo_error),
            },
        ],
        init: None,
        types: vec![TypeDef::Struct(StructDef {
            name: "EchoError".to_owned(),
            fields: vec![Field {
                name: "reason".to_owned(),
                ty: TypeRef::String,
            }],
        })],
    }
}

fn notifier_contract() -> Contract {
    Contract {
        plugin_id: "dev.istmo.demo.notifier".to_owned(),
        type_name: "Notifier".to_owned(),
        methods: vec![Method {
            name: "notify".to_owned(),
            kind: MethodKind::Unary,
            args: vec![istmo_build::Arg {
                name: "message".to_owned(),
                ty: TypeRef::String,
            }],
            returns: TypeRef::Unit,
            error: Some(TypeRef::Named("EchoError".to_owned())),
        }],
        init: None,
        // Empty: EchoError comes from EchoTypes.kt in the same package.
        types: vec![],
    }
}

fn with_package(package: &str, body: &str) -> String {
    format!("package {package}\n\n{body}")
}

fn with_package_and_imports(package: &str, body: &str) -> String {
    let imports = "\
import dev.istmo.runtime.BackendException\n\
import dev.istmo.runtime.Bincode\n\
import dev.istmo.runtime.HandleReleaser\n\
import dev.istmo.runtime.IstmoRuntime\n\
import dev.istmo.runtime.PluginException\n\
import dev.istmo.runtime.PluginHandler\n";
    format!("package {package}\n\n{imports}\n{body}")
}

fn emit(dest: &Path, contents: &str) {
    if let Err(err) = write_if_changed(dest, contents) {
        println!(
            "cargo:warning=android-demo build.rs: failed to write {}: {err}",
            dest.display()
        );
    }
}

fn write_if_changed(dest: &Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Ok(existing) = fs::read_to_string(dest) {
        if existing == contents {
            return Ok(());
        }
    }
    fs::write(dest, contents)
}
