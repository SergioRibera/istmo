//! Pins the Kotlin / Swift declarations `istmo-build` generates for the
//! shared media types (once per app). Plugin native code compiles
//! against them (`scripts/check-plugin-native.sh`).
//!
//! Refresh after an intended change with
//! `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-media --test codegen_golden`.

use std::path::PathBuf;

use istmo_build::{
    SharedTypes, extract_shared_types, generate_kotlin_shared_types, generate_swift_shared_types,
};

fn shared() -> SharedTypes {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");
    extract_shared_types(src, "IstmoMedia", "dev.istmo.media").expect("shared types")
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
         `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-media --test codegen_golden`",
    );
}

#[test]
fn kotlin_types() {
    assert_golden(
        "istmo_media_types.kt",
        &generate_kotlin_shared_types(&shared()),
    );
}

#[test]
fn swift_types() {
    assert_golden(
        "istmo_media_types.swift",
        &generate_swift_shared_types(&shared()),
    );
}
