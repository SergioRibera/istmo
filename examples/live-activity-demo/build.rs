//! Emits the Kotlin + Swift host-side glue for the `LiveActivity`
//! contract into the Android and iOS source trees.
//!
//! Runs on every `cargo build` regardless of target — text files are
//! cheap to write and living in the source tree means Gradle and Xcode
//! pick them up without extra glue. The generated files are `.gitignore`d.

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{
    Contract, EnumDef, EnumVariant, Field, StructDef, TypeDef, TypeRef, generate_kotlin_codecs,
    generate_kotlin_host, generate_kotlin_types, generate_swift_codecs, generate_swift_host,
    generate_swift_types,
};
use istmo_live_activity::codegen as live_activity_contract;

/// Demo-side `#[istmo::message]` types (`TimerAttributes`, `TimerState`)
/// wrapped in a methodless [`Contract`] so `generate_*_types` and
/// `generate_*_codecs` will emit their Kotlin / Swift bindings alongside
/// the live-activity plugin's own generated files.
///
/// `plugin_id` and `type_name` here are labels only — no dispatcher is
/// generated when `methods` is empty.
fn demo_types_contract() -> Contract {
    Contract {
        plugin_id: "demo.live_activity".to_owned(),
        type_name: "Demo".to_owned(),
        methods: vec![],
        init: None,
        types: vec![
            TypeDef::Struct(StructDef {
                name: "TimerAttributes".to_owned(),
                fields: vec![
                    Field {
                        name: "title".to_owned(),
                        ty: TypeRef::String,
                    },
                    Field {
                        name: "target_seconds".to_owned(),
                        ty: TypeRef::U32,
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "TimerState".to_owned(),
                fields: vec![
                    Field {
                        name: "elapsed_seconds".to_owned(),
                        ty: TypeRef::U32,
                    },
                    Field {
                        name: "label".to_owned(),
                        ty: TypeRef::String,
                    },
                ],
            }),
            // Force the codec generator to walk both TimerAttributes and
            // TimerState — every `Named` type must be reachable from at
            // least one method arg / return in the contract to trigger a
            // codec entry. Since this spec has no methods, add a dummy
            // `Marker` enum whose variants reference both structs.
            TypeDef::Enum(EnumDef {
                name: "TimerCodecMarker".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Attributes".to_owned(),
                        payload: vec![TypeRef::Named("TimerAttributes".to_owned())],
                    },
                    EnumVariant {
                        name: "State".to_owned(),
                        payload: vec![TypeRef::Named("TimerState".to_owned())],
                    },
                ],
            }),
        ],
    }
}

fn main() {
    let _wiring = istmo_build::emit_wiring_env(None);

    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let android_generated = root.join("android/app/src/main/java/dev/istmo/runtime");
    let ios_plugins = root.join("ios/LiveActivityDemo/Plugins");

    for spec in dispatchers() {
        let type_name = &spec.contract.type_name;

        // ---- iOS -----------------------------------------------------
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

        // ---- Android -------------------------------------------------
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
    println!("cargo:rerun-if-changed=../../plugins/live-activity/src/codegen.rs");
    println!("cargo:rerun-if-changed=../../plugins/live-activity/src/lib.rs");
}

fn emit(dest: &Path, source: &str) {
    if let Err(err) = write_if_changed(dest, source) {
        println!(
            "cargo:warning=live-activity-demo build.rs: failed to write {}: {err}",
            dest.display()
        );
    }
}

/// The Kotlin generator emits imports but no `package` header. Prepend
/// `package dev.istmo.runtime` so the dispatcher sits alongside
/// `PluginHandler`, `Bincode`, etc.
fn kotlin_source_with_package(body: &str) -> String {
    format!("package dev.istmo.runtime\n\n{body}")
}

struct DispatcherSpec {
    dir_name: &'static str,
    contract: Contract,
}

fn dispatchers() -> Vec<DispatcherSpec> {
    vec![
        DispatcherSpec {
            dir_name: "LiveActivity",
            contract: live_activity_contract::contract(),
        },
        DispatcherSpec {
            dir_name: "Demo",
            contract: demo_types_contract(),
        },
    ]
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
