use std::fs;

use istmo_build::{AppOpts, Manifest, emit_app_with};

const SERVICE_MANIFEST: &str = r#"
[plugin]
id = "myapp.sync"

[plugin.android_service]
class_name = "SyncForegroundService"
foreground_service_type = "dataSync"
exported = false

[plugin.ios_background]
class_name = "SyncBackgroundHandler"
task_identifier = "com.myapp.sync.refresh"
kind = "refresh"
interval_minutes = 15
"#;

fn scratch_root(subdir: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!("istmo-emit-app-{}-{}", subdir, std::process::id(),));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

#[test]
fn emit_app_writes_kotlin_service_class_and_sidecar() {
    let root = scratch_root("android-service");
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::create_dir_all(android_root.join("app")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.app\"\n}\n",
    )
    .unwrap();

    let manifest = Manifest::parse(SERVICE_MANIFEST).unwrap();
    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.extra_manifests = vec![manifest];
    opts.lib_name = Some("myapp".to_owned());
    opts.extra_contracts = vec![]; // ensure `!contracts.is_empty()` short-circuit is bypassed via manifest side

    // emit_app currently early-returns if contracts is empty; feed a dummy
    // contract so the flow reaches the service/background emit.
    opts.extra_contracts.push(dummy_contract());

    emit_app_with(opts);

    let kotlin =
        android_root.join("app/src/main/java/com/example/app/gen/SyncForegroundService.kt");
    assert!(
        kotlin.is_file(),
        "kotlin class file expected at {}",
        kotlin.display()
    );
    let contents = fs::read_to_string(&kotlin).unwrap();
    assert!(contents.contains("class SyncForegroundService"));
    assert!(contents.contains("const val PLUGIN_ID: String = \"myapp.sync\""));
    assert!(contents.contains("private const val LIBRARY_NAME: String = \"myapp\""));

    let sidecar = android_root.join("app/src/main/AndroidManifest.services.xml");
    assert!(sidecar.is_file());
    let sidecar_body = fs::read_to_string(&sidecar).unwrap();
    assert!(sidecar_body.contains("<service"));
    assert!(sidecar_body.contains("com.example.app.gen.SyncForegroundService"));
    assert_well_formed_comments(&sidecar_body);
    assert!(sidecar_body.contains("android:foregroundServiceType=\"dataSync\""));

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn emit_app_writes_swift_bgtask_and_sidecar() {
    let root = scratch_root("ios-bg");
    let ios_root = root.join("ios");
    fs::create_dir_all(ios_root.join("MyApp")).unwrap();
    fs::create_dir_all(ios_root.join("MyApp.xcodeproj")).unwrap();

    let manifest = Manifest::parse(SERVICE_MANIFEST).unwrap();
    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.ios_root = Some(ios_root.clone());
    opts.android = Some(false);
    opts.extra_manifests = vec![manifest];
    opts.extra_contracts = vec![dummy_contract()];

    emit_app_with(opts);

    let swift = ios_root.join("MyApp/Plugins/Background/SyncBackgroundHandler.swift");
    assert!(swift.is_file(), "swift class file at {}", swift.display());
    let contents = fs::read_to_string(&swift).unwrap();
    assert!(contents.contains("public enum SyncBackgroundHandler"));
    assert!(contents.contains("TASK_ID: String = \"com.myapp.sync.refresh\""));
    assert!(contents.contains("BGAppRefreshTaskRequest"));

    let sidecar = ios_root.join("MyApp/Info.plist.background.xml");
    assert!(sidecar.is_file());
    let sidecar_body = fs::read_to_string(&sidecar).unwrap();
    assert!(sidecar_body.contains("BGTaskSchedulerPermittedIdentifiers"));
    assert!(sidecar_body.contains("com.myapp.sync.refresh"));
    assert_well_formed_comments(&sidecar_body);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn emit_app_patches_android_manifest_between_markers() {
    let root = scratch_root("android-patch");
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.app\"\n}\n",
    )
    .unwrap();
    let android_manifest_path = android_root.join("app/src/main/AndroidManifest.xml");
    fs::write(
        &android_manifest_path,
        r#"<?xml version="1.0" encoding="utf-8"?>
<manifest xmlns:android="http://schemas.android.com/apk/res/android">
    <application>
        <activity android:name=".MainActivity" />
        <!-- istmo:services:start -->
        <!-- istmo:services:end -->
    </application>
</manifest>
"#,
    )
    .unwrap();

    let manifest = Manifest::parse(SERVICE_MANIFEST).unwrap();
    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.extra_manifests = vec![manifest];
    opts.lib_name = Some("myapp".to_owned());
    opts.extra_contracts = vec![dummy_contract()];

    emit_app_with(opts);

    let patched = fs::read_to_string(&android_manifest_path).unwrap();
    let start = patched.find("<!-- istmo:services:start -->").unwrap();
    let end = patched.find("<!-- istmo:services:end -->").unwrap();
    let managed = &patched[start..end];
    assert!(
        managed.contains("com.example.app.gen.SyncForegroundService"),
        "managed block should carry service element, got: {managed}",
    );

    // Idempotent: second run must not grow the managed block.
    let manifest = Manifest::parse(SERVICE_MANIFEST).unwrap();
    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.extra_manifests = vec![manifest];
    opts.lib_name = Some("myapp".to_owned());
    opts.extra_contracts = vec![dummy_contract()];
    emit_app_with(opts);
    let second = fs::read_to_string(&android_manifest_path).unwrap();
    assert_eq!(patched, second);

    let _ = fs::remove_dir_all(&root);
}

#[test]
fn emit_app_patches_ios_info_plist_between_markers() {
    let root = scratch_root("ios-patch");
    let ios_root = root.join("ios");
    fs::create_dir_all(ios_root.join("MyApp")).unwrap();
    fs::create_dir_all(ios_root.join("MyApp.xcodeproj")).unwrap();
    let plist_path = ios_root.join("MyApp/Info.plist");
    fs::write(
        &plist_path,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <key>CFBundleIdentifier</key>
    <string>com.example.app</string>
    <!-- istmo:background:start -->
    <!-- istmo:background:end -->
</dict>
</plist>
"#,
    )
    .unwrap();

    let manifest = Manifest::parse(SERVICE_MANIFEST).unwrap();
    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.ios_root = Some(ios_root.clone());
    opts.android = Some(false);
    opts.extra_manifests = vec![manifest];
    opts.extra_contracts = vec![dummy_contract()];

    emit_app_with(opts);

    let patched = fs::read_to_string(&plist_path).unwrap();
    let start = patched.find("<!-- istmo:background:start -->").unwrap();
    let end = patched.find("<!-- istmo:background:end -->").unwrap();
    let managed = &patched[start..end];
    assert!(managed.contains("BGTaskSchedulerPermittedIdentifiers"));
    assert!(managed.contains("com.myapp.sync.refresh"));

    let _ = fs::remove_dir_all(&root);
}

const INFO_PLIST_MANIFEST: &str = r#"
[plugin]
id = "istmo.biometric"

[plugin.info_plist]
NSFaceIDUsageDescription = "Plugin default reason"
"#;

const OTHER_INFO_PLIST_MANIFEST: &str = r#"
[plugin]
id = "istmo.other"

[plugin.info_plist]
NSFaceIDUsageDescription = "Conflicting reason"
UIFileSharingEnabled = true
"#;

#[test]
fn emit_app_merges_plugin_info_plist_entries_with_app_overrides() {
    let root = scratch_root("ios-info-plist");
    let ios_root = root.join("ios");
    fs::create_dir_all(ios_root.join("MyApp")).unwrap();
    fs::create_dir_all(ios_root.join("MyApp.xcodeproj")).unwrap();
    let plist_path = ios_root.join("MyApp/Info.plist");
    fs::write(
        &plist_path,
        r#"<?xml version="1.0" encoding="UTF-8"?>
<plist version="1.0">
<dict>
    <!-- istmo:background:start -->
    <!-- istmo:background:end -->
</dict>
</plist>
"#,
    )
    .unwrap();

    let emit = |app_toml: &str| {
        fs::write(root.join("istmo.toml"), app_toml).unwrap();
        let mut opts = AppOpts::default();
        opts.root = Some(root.clone());
        opts.ios_root = Some(ios_root.clone());
        opts.android = Some(false);
        opts.extra_manifests = vec![
            Manifest::parse(INFO_PLIST_MANIFEST).unwrap(),
            Manifest::parse(OTHER_INFO_PLIST_MANIFEST).unwrap(),
        ];
        opts.extra_contracts = vec![dummy_contract()];
        emit_app_with(opts);
        fs::read_to_string(&plist_path).unwrap()
    };

    // First plugin wins on conflicting keys; unrelated keys are kept.
    let patched = emit("[app]\n");
    assert!(
        patched.contains(
            "<key>NSFaceIDUsageDescription</key>\n<string>Plugin default reason</string>"
        )
    );
    assert!(!patched.contains("Conflicting reason"));
    assert!(patched.contains("<key>UIFileSharingEnabled</key>\n<true/>"));
    let sidecar = fs::read_to_string(ios_root.join("MyApp/Info.plist.background.xml")).unwrap();
    assert!(sidecar.contains("Plugin default reason"));
    assert_well_formed_comments(&sidecar);

    // `[app.info_plist]` overrides the plugin default.
    let patched = emit("[app]\n\n[app.info_plist]\nNSFaceIDUsageDescription = \"App reason\"\n");
    assert!(patched.contains("<string>App reason</string>"));
    assert!(!patched.contains("Plugin default reason"));
    assert_eq!(patched.matches("NSFaceIDUsageDescription").count(), 1);

    let _ = fs::remove_dir_all(&root);
}

/// XML comments do not nest: every `<!--` must be closed by `-->`
/// before the next `<!--` opens, and `--` may not appear inside.
fn assert_well_formed_comments(xml: &str) {
    let mut rest = xml;
    while let Some(open) = rest.find("<!--") {
        let after = &rest[open + 4..];
        let close = after
            .find("-->")
            .unwrap_or_else(|| panic!("unterminated comment in:\n{xml}"));
        let body = &after[..close];
        assert!(
            !body.contains("<!--") && !body.contains("--"),
            "malformed comment `{body}` in:\n{xml}"
        );
        rest = &after[close + 3..];
    }
}

fn dummy_contract() -> istmo_build::Contract {
    istmo_build::Contract {
        plugin_id: "myapp.dummy".to_owned(),
        type_name: "Dummy".to_owned(),
        methods: vec![],
        init: None,
        types: vec![],
    }
}
