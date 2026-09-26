//! Pins the Kotlin / Swift code `istmo-build` generates for the
//! `Share` contract. The native backends under `native/` are written
//! against this output, so an unnoticed generator or contract change
//! would break them silently.
//!
//! Refresh after an intended change with
//! `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-share --test codegen_golden`.

use std::path::PathBuf;

use istmo_build::{
    Contract, extract_contract, generate_kotlin_client, generate_kotlin_codecs,
    generate_kotlin_host, generate_kotlin_types, generate_swift_client, generate_swift_codecs,
    generate_swift_host, generate_swift_types,
};

fn contract() -> Contract {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");
    extract_contract(src, "Share").expect("extract Share contract")
}

fn assert_golden(file: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{file}.expected"));
    if std::env::var_os("ISTMO_BUILD_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, actual).expect("update golden");
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    assert_eq!(
        actual, expected,
        "golden mismatch for {file}; refresh with \
         `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-share --test codegen_golden`",
    );
}

#[test]
fn kotlin_types() {
    assert_golden("share_types.kt", &generate_kotlin_types(&contract()));
}

#[test]
fn kotlin_host() {
    assert_golden("share_host.kt", &generate_kotlin_host(&contract()));
}

#[test]
fn kotlin_codecs() {
    assert_golden("share_codecs.kt", &generate_kotlin_codecs(&contract()));
}

#[test]
fn kotlin_client() {
    assert_golden("share_client.kt", &generate_kotlin_client(&contract()));
}

#[test]
fn swift_types() {
    assert_golden("share_types.swift", &generate_swift_types(&contract()));
}

#[test]
fn swift_host() {
    assert_golden("share_host.swift", &generate_swift_host(&contract()));
}

#[test]
fn swift_codecs() {
    assert_golden("share_codecs.swift", &generate_swift_codecs(&contract()));
}

#[test]
fn swift_client() {
    assert_golden("share_client.swift", &generate_swift_client(&contract()));
}
