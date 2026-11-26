//! Pins the Swift code `istmo-build` generates for the `Biometric`
//! contract, and checks that it shares one iOS app with the camera and
//! screen-capture plugins: `Availability`, `SessionConfig` and
//! `PixelFormat` are declared by more than one of them, each in its own
//! namespace. `runtime/ios/ci/typecheck-goldens.sh` type-checks the three
//! sets of goldens together.
//!
//! Refresh after an intended change with
//! `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-biometric --test codegen_golden`.

use std::path::PathBuf;

use istmo_build::{
    Contract, check_swift_namespaces, extract_contract, extract_contract_with_shared,
    extract_shared_types, generate_swift_codecs, generate_swift_host, generate_swift_types,
};

fn plugin_source(dir: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join(dir)
        .join("src/lib.rs")
}

fn contract() -> Contract {
    extract_contract(plugin_source("biometric"), "Biometric").expect("extract Biometric contract")
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
         `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-biometric --test codegen_golden`",
    );
}

#[test]
fn swift_types() {
    assert_golden("biometric_types.swift", &generate_swift_types(&contract()));
}

#[test]
fn swift_host() {
    assert_golden("biometric_host.swift", &generate_swift_host(&contract()));
}

#[test]
fn swift_codecs() {
    assert_golden(
        "biometric_codecs.swift",
        &generate_swift_codecs(&contract()),
    );
}

/// Whether `contract` declares a message type called `name`.
fn declares(contract: &Contract, name: &str) -> bool {
    contract.types.iter().any(|ty| ty.name() == name)
}

#[test]
fn shares_an_ios_app_with_camera_and_screen_capture() {
    let media = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../crates/media/src/lib.rs");
    let media = extract_shared_types(media, "IstmoMedia", "dev.istmo.media").expect("istmo-media");
    let camera = extract_contract_with_shared(
        plugin_source("camera"),
        "Camera",
        std::slice::from_ref(&media),
    )
    .expect("extract Camera contract");
    let capture =
        extract_contract_with_shared(plugin_source("screen-capture"), "ScreenCapture", &[media])
            .expect("extract ScreenCapture contract");
    let biometric = contract();
    // The names the namespaces keep apart.
    assert!(declares(&biometric, "Availability") && declares(&capture, "Availability"));
    for name in ["SessionConfig", "PixelFormat"] {
        assert!(
            declares(&camera, name) && declares(&capture, name),
            "{name}"
        );
    }
    check_swift_namespaces(&[&camera, &capture, &biometric]).expect("no Swift name clash");
    assert!(generate_swift_types(&biometric).contains("extension Biometric {"));
}
