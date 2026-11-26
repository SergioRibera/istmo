//! Guards the plugin's `istmo.toml`: the Android backend declares what
//! MediaProjection needs and nothing more; the not-yet-capturing iOS
//! backend declares nothing.

use istmo_build::{AndroidBackendArg, Manifest};

fn manifest() -> Manifest {
    Manifest::from_path(concat!(env!("CARGO_MANIFEST_DIR"), "/istmo.toml"))
        .expect("plugin istmo.toml parses")
}

#[test]
fn android_declares_the_projection_service_permissions_only() {
    let manifest = manifest();
    let plugin = &manifest.plugins[0];
    let mut names: Vec<&str> = plugin
        .android_permissions
        .iter()
        .map(|p| p.name.as_str())
        .collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "android.permission.FOREGROUND_SERVICE",
            "android.permission.FOREGROUND_SERVICE_MEDIA_PROJECTION",
            "android.permission.POST_NOTIFICATIONS",
        ]
    );
    // Audio capture needs the microphone permission; apps that capture
    // audio declare it themselves.
    assert!(!names.contains(&"android.permission.RECORD_AUDIO"));
    assert!(matches!(
        plugin.android_backend_arg,
        AndroidBackendArg::Activity(_)
    ));
}

#[test]
fn android_ships_the_projection_service() {
    let manifest = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/native/android/AndroidManifest.xml"
    ))
    .expect("native/android/AndroidManifest.xml");
    assert!(manifest.contains("dev.istmo.plugins.screencapture.IstmoProjectionService"));
    assert!(manifest.contains("android:exported=\"false\""));
    assert!(manifest.contains("android:foregroundServiceType=\"mediaProjection\""));
}

#[test]
fn ios_declares_nothing_before_it_captures() {
    assert_eq!(manifest().plugins[0].info_plist.len(), 0);
}

#[test]
fn ios_floor_matches_the_runtime() {
    let ios = manifest().min_versions.ios.expect("ios minimum declared");
    assert_eq!(ios.to_string(), "14.0");
}
