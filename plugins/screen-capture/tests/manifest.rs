//! Guards the plugin's `istmo.toml`: nothing is declared that the
//! not-yet-capturing mobile backends do not use.

use istmo_build::Manifest;

fn manifest() -> Manifest {
    Manifest::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/istmo.toml"))
        .expect("plugin istmo.toml parses")
}

#[test]
fn declares_no_permissions_before_the_backends_capture() {
    let manifest = manifest();
    assert!(manifest.plugins[0].android_permissions.is_empty());
    assert!(manifest.plugins[0].info_plist.is_empty());
}

#[test]
fn ios_floor_matches_the_runtime() {
    let ios = manifest().min_versions.ios.expect("ios minimum declared");
    assert_eq!(ios.to_string(), "14.0");
}
