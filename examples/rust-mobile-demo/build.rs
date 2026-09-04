//! Regenerates the Swift host dispatcher for every plugin the demo
//! hosts on iOS.
//!
//! Runs on every `cargo build` regardless of target — writing static
//! text files is cheap, and the output living in the source tree means
//! Xcode picks it up without extra glue. The emitted files land under
//! `ios/RustMobileDemo/Plugins/<Name>/Generated/`; that path is
//! git-ignored (see the workspace-root `.gitignore`) so a checkout after
//! `cargo build` stays clean.
//!
//! The build.rs consumes `istmo_plugins::contract::*` builders, which
//! return `istmo_build::Contract` values byte-identical to the golden
//! fixtures in `crates/build/tests/golden/`. Any Rust-side change to a
//! plugin trait shape flows through:
//!
//! 1. Update the trait in `crates/plugins/src/<name>.rs`.
//! 2. Update `crates/plugins/src/contract.rs` to match.
//! 3. Refresh `crates/build/tests/golden/` via
//!    `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-build`.
//! 4. `cargo build -p rust-mobile-demo` regenerates the Swift.
//!
//! Errors are surfaced as `cargo:warning=…` so the compiler still
//! completes on a first-time checkout that has never had ios/Plugins/*
//! directories.

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{Contract, generate_swift_host};
use istmo_plugins::contract as plugin_contract;

fn main() {
    // Anchor everything at the demo's own directory. `CARGO_MANIFEST_DIR`
    // is the crate root regardless of who invoked cargo (Gradle, Xcode
    // pre-build script, plain `cargo build --workspace`).
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let plugins_dir = root.join("ios/RustMobileDemo/Plugins");

    for spec in dispatchers() {
        let dest_dir = plugins_dir.join(spec.dir_name).join("Generated");
        let dest = dest_dir.join(format!("{}Dispatcher.swift", spec.contract.type_name));
        let generated = generate_swift_host(&spec.contract);
        if let Err(err) = write_if_changed(&dest, &generated) {
            println!(
                "cargo:warning=rust-mobile-demo build.rs: failed to write {}: {err}",
                dest.display()
            );
        }
    }

    // Only rerun when the codegen sources change. The Rust plugin traits
    // themselves live under `crates/plugins`, so watch that dir plus the
    // generator crate.
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../../crates/plugins/src");
    println!("cargo:rerun-if-changed=../../crates/build/src");
}

struct DispatcherSpec {
    dir_name: &'static str,
    contract: Contract,
}

fn dispatchers() -> Vec<DispatcherSpec> {
    vec![
        DispatcherSpec {
            dir_name: "Permissions",
            contract: plugin_contract::permissions(),
        },
        DispatcherSpec {
            dir_name: "Notifications",
            contract: plugin_contract::notifications(),
        },
        DispatcherSpec {
            dir_name: "SignIn",
            contract: plugin_contract::google_sign_in(),
        },
        DispatcherSpec {
            dir_name: "AdMob",
            contract: plugin_contract::admob(),
        },
    ]
}

/// Write `contents` to `dest`, but only if the on-disk bytes differ.
/// Skipping unchanged writes keeps Xcode's `input dependencies` map
/// clean — every touched file cascades into a Swift recompile.
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
