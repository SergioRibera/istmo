//! Guards the plugin's `istmo.toml` against regressions that only show
//! up on a device (a missing permission, a stray usage string).

use istmo_build::Manifest;

fn manifest() -> Manifest {
    Manifest::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/istmo.toml"))
        .expect("plugin istmo.toml parses")
}

#[test]
fn declares_android_camera_permission() {
    let manifest = manifest();
    let permissions = &manifest.plugins[0].android_permissions;
    assert!(
        permissions
            .iter()
            .any(|p| p.name == "android.permission.CAMERA" && p.max_sdk_version.is_none()),
        "{permissions:?}"
    );
}
