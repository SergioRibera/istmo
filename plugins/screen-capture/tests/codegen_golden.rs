//! Pins the Kotlin / Swift code `istmo-build` generates for the
//! `ScreenCapture` contract. The native backends under `native/` are
//! written against this output, so an unnoticed generator or contract
//! change would break them silently.
//!
//! Refresh after an intended change with
//! `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-screen-capture --test codegen_golden`.

use std::path::PathBuf;

use istmo_build::{
    Contract, extract_contract_with_shared, extract_shared_types, generate_kotlin_client,
    generate_kotlin_codecs, generate_kotlin_host, generate_kotlin_types, generate_swift_client,
    generate_swift_codecs, generate_swift_host, generate_swift_types,
};

fn contract() -> Contract {
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lib.rs");
    let media = concat!(env!("CARGO_MANIFEST_DIR"), "/../../crates/media/src/lib.rs");
    let media = extract_shared_types(media, "IstmoMedia", "dev.istmo.media").expect("istmo-media");
    extract_contract_with_shared(src, "ScreenCapture", &[media])
        .expect("extract ScreenCapture contract")
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
         `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-screen-capture --test codegen_golden`",
    );
}

#[test]
fn kotlin_types() {
    assert_golden(
        "screen_capture_types.kt",
        &generate_kotlin_types(&contract()),
    );
}

#[test]
fn kotlin_host() {
    assert_golden("screen_capture_host.kt", &generate_kotlin_host(&contract()));
}

#[test]
fn kotlin_codecs() {
    assert_golden(
        "screen_capture_codecs.kt",
        &generate_kotlin_codecs(&contract()),
    );
}

#[test]
fn kotlin_client() {
    assert_golden(
        "screen_capture_client.kt",
        &generate_kotlin_client(&contract()),
    );
}

#[test]
fn swift_types() {
    assert_golden(
        "screen_capture_types.swift",
        &generate_swift_types(&contract()),
    );
}

#[test]
fn swift_host() {
    assert_golden(
        "screen_capture_host.swift",
        &generate_swift_host(&contract()),
    );
}

#[test]
fn swift_codecs() {
    assert_golden(
        "screen_capture_codecs.swift",
        &generate_swift_codecs(&contract()),
    );
}

#[test]
fn swift_client() {
    assert_golden(
        "screen_capture_client.swift",
        &generate_swift_client(&contract()),
    );
}

#[test]
fn clients_drop_the_sink_taking_methods() {
    let kotlin = generate_kotlin_client(&contract());
    for method in [
        "screenshot(",
        "open(",
        "open_with_audio(",
        "open_preview_with_frames(",
    ] {
        assert!(
            !kotlin.contains(&format!("fun {method}")),
            "{method} leaked into the Kotlin client"
        );
    }
    assert!(kotlin.contains("fun screenshot_pixels("));
    assert!(kotlin.contains("fun open_preview("));
}
