use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{
    Contract, Field, Method, MethodKind, StructDef, TypeDef, TypeRef, generate_kotlin_client,
    generate_kotlin_codecs, generate_kotlin_codecs_interface, generate_kotlin_host,
    generate_kotlin_types,
};
use istmo_plugins_schema as plugin_contract;

fn main() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    let gen_dir = root
        .join("../../android/app/src/main/java/dev/istmo/multi/gen")
        .canonicalize()
        .unwrap_or_else(|_| root.join("../../android/app/src/main/java/dev/istmo/multi/gen"));

    emit_client_bundle(&gen_dir, "dev.istmo.multi.gen", &app_control_contract());

    emit_host_bundle(
        &gen_dir,
        "dev.istmo.multi.gen",
        &plugin_contract::service_control(),
    );

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

fn emit_host_bundle(gen_dir: &Path, package: &str, contract: &Contract) {
    let name = contract.type_name.as_str();
    emit(
        &gen_dir.join(format!("{name}Types.kt")),
        &with_package(package, &generate_kotlin_types(contract)),
    );
    emit(
        &gen_dir.join(format!("{name}Host.kt")),
        &with_package_and_imports(package, &generate_kotlin_host(contract)),
    );
    emit(
        &gen_dir.join(format!("{name}CodecsImpl.kt")),
        &with_package(package, &generate_kotlin_codecs(contract)),
    );
}

fn app_control_contract() -> Contract {
    Contract {
        plugin_id: "dev.istmo.multi.app_control".to_owned(),
        type_name: "AppControl".to_owned(),
        methods: vec![Method {
            name: "ping".to_owned(),
            kind: MethodKind::Unary,
            args: vec![],
            returns: TypeRef::String,
            error: Some(TypeRef::Named("MultiError".to_owned())),
        }],
        init: None,
        types: vec![TypeDef::Struct(StructDef {
            name: "MultiError".to_owned(),
            fields: vec![Field {
                name: "reason".to_owned(),
                ty: TypeRef::String,
            }],
        })],
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
            "cargo:warning=android-multi build.rs: failed to write {}: {err}",
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

