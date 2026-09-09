//! Emits the `SignIn` contract, native-dep declarations and regenerates the
//! `#[message]` type shapes from the canonical `Contract` in
//! `crate::codegen::contract`.
//!
//! Every serious istmo plugin ends up with this shape — the pattern is
//! reproduced verbatim inside `project-template`'s plugin skeleton so
//! community authors can crib it as a starting point.

use std::path::PathBuf;

use istmo_build::{emit_contract, emit_manifest_metadata, generate_rust_types};

#[path = "src/codegen.rs"]
mod codegen;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/codegen.rs");

    // Publish native deps + plugin id (`DEP_ISTMO_GOOGLE_SIGN_IN_*`) through
    // the standard cross-crate metadata channel. Downstream app's `build.rs`
    // picks them up via `istmo_build::collect_dep_native_deps()`.
    emit_manifest_metadata("istmo.toml");

    // Publish the `Contract` so downstream apps' `build.rs` can codegen the
    // Kotlin / Swift host without hand-authoring a matching contract shape.
    let contract = codegen::contract();
    emit_contract(&contract);

    // Regenerate the `#[message]` type declarations included from `lib.rs`.
    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR"));
    let dest = out_dir.join("google_sign_in_types.rs");
    let source = generate_rust_types(&contract);
    if let Err(err) = std::fs::write(&dest, source) {
        panic!(
            "istmo-google-sign-in build.rs: failed to write {}: {err}",
            dest.display(),
        );
    }
}
