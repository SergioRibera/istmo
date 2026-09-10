//! Emits the Kotlin + Swift host-side glue for the `DataStore` contract
//! into the Android and iOS source trees.
//!
//! Runs on every `cargo build` regardless of target — text files are cheap
//! to write and living in the source tree means Gradle and Xcode pick
//! them up without extra glue. The generated files are `.gitignore`d.
//!
//! Flow:
//!
//! 1. `emit_wiring_env(None)` walks `DEP_*_ISTMO_MANIFEST` env vars set by
//!    dependency `build.rs` scripts, applies our `istmo.toml` remote
//!    overrides (none for this demo) and exports `ISTMO_AUTO_PLUGINS` /
//!    `ISTMO_AUTO_REMOTE` — the `istmo::runtime!` macro reads them at
//!    expansion time.
//! 2. For every dispatcher spec, write:
//!    * `<Trait>Dispatcher.kt/.swift` — codegen host with wire encode /
//!      decode.
//!    * `<Trait>Types.kt/.swift` — data classes / structs / enums.
//!    * `<Trait>CodecsImpl.kt/.swift` — per-type bincode codecs.

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{
    Contract, generate_kotlin_codecs, generate_kotlin_host, generate_kotlin_types,
    generate_swift_codecs, generate_swift_host, generate_swift_types,
};
use istmo_data_store::codegen as data_store_contract;

fn main() {
    let _wiring = istmo_build::emit_wiring_env(None);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let android_generated = root.join("android/app/src/main/java/dev/istmo/runtime");
    let ios_plugins = root.join("ios/DataStoreDemo/Plugins");

    for spec in dispatchers() {
        let type_name = &spec.contract.type_name;

        // ---- iOS: dispatcher + types + codecs -----------------------
        let ios_generated = ios_plugins.join(spec.dir_name).join("Generated");
        emit(
            &ios_generated.join(format!("{type_name}Dispatcher.swift")),
            &generate_swift_host(&spec.contract),
        );
        emit(
            &ios_generated.join(format!("{type_name}Types.swift")),
            &generate_swift_types(&spec.contract),
        );
        emit(
            &ios_generated.join(format!("{type_name}CodecsImpl.swift")),
            &generate_swift_codecs(&spec.contract),
        );

        // ---- Android: dispatcher + types + codecs -------------------
        emit(
            &android_generated.join(format!("{type_name}Dispatcher.kt")),
            &kotlin_source_with_package(&generate_kotlin_host(&spec.contract)),
        );
        emit(
            &android_generated.join(format!("{type_name}Types.kt")),
            &kotlin_source_with_package(&generate_kotlin_types(&spec.contract)),
        );
        emit(
            &android_generated.join(format!("{type_name}CodecsImpl.kt")),
            &kotlin_source_with_package(&generate_kotlin_codecs(&spec.contract)),
        );
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../plugins/data-store/src/codegen.rs");
    println!("cargo:rerun-if-changed=../../plugins/data-store/src/lib.rs");
}

fn emit(dest: &Path, source: &str) {
    if let Err(err) = write_if_changed(dest, source) {
        println!(
            "cargo:warning=data-store-demo build.rs: failed to write {}: {err}",
            dest.display()
        );
    }
}

/// The Kotlin generator emits imports but no `package` header. Prepend
/// `package dev.istmo.runtime` so the dispatcher sits alongside
/// `PluginHandler`, `Bincode` — the class does not need explicit imports
/// for its siblings.
fn kotlin_source_with_package(body: &str) -> String {
    format!("package dev.istmo.runtime\n\n{body}")
}

struct DispatcherSpec {
    dir_name: &'static str,
    contract: Contract,
}

fn dispatchers() -> Vec<DispatcherSpec> {
    vec![DispatcherSpec {
        dir_name: "DataStore",
        contract: data_store_contract::contract(),
    }]
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
