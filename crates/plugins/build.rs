use std::path::PathBuf;

use istmo_build::{Contract, generate_rust_types};

fn main() {
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));

    emit(
        &out_dir.join("permissions_types.rs"),
        &istmo_plugins_schema::permissions(),
    );
    emit(
        &out_dir.join("notifications_types.rs"),
        &istmo_plugins_schema::notifications(),
    );
    emit(
        &out_dir.join("admob_types.rs"),
        &istmo_plugins_schema::admob(),
    );

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../plugins-schema/src");
    println!("cargo:rerun-if-changed=../build/src");
}

fn emit(dest: &std::path::Path, contract: &Contract) {
    let source = generate_rust_types(contract);
    if let Err(err) = std::fs::write(dest, source) {
        panic!(
            "istmo-plugins build.rs: failed to write {}: {err}",
            dest.display(),
        );
    }
}

