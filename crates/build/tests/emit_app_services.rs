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

fn dummy_contract() -> istmo_build::Contract {
    istmo_build::Contract {
        plugin_id: "myapp.dummy".to_owned(),
        type_name: "Dummy".to_owned(),
        methods: vec![],
        init: None,
        types: vec![],
    }
}

#[test]
fn emit_app_merges_plugin_plist_fragments_into_marked_info_plist() {
    let root = scratch_root("ios-plist-fragments");
    let ios_root = root.join("ios");
    let app_dir = ios_root.join("MyApp");
    fs::create_dir_all(&app_dir).unwrap();
    fs::write(
        app_dir.join("Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\">\n<dict>\n\
         \t<key>CFBundleName</key>\n\t<string>MyApp</string>\n\
         \t<!-- istmo:plugins:start -->\n\t<!-- istmo:plugins:end -->\n</dict>\n</plist>\n",
    )
    .unwrap();

    let plugin_dir = root.join("plugin-native-ios");
    fs::create_dir_all(&plugin_dir).unwrap();
    fs::write(
        plugin_dir.join("Info.plist.fragment"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>NSPhotoLibraryAddUsageDescription</key><string>Save shared images</string>\
         <key>CFBundleName</key><string>Ignored</string>\
         </dict></plist>\n",
    )
    .unwrap();
    fs::write(
        plugin_dir.join("App.entitlements.fragment"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
         <key>com.apple.security.application-groups</key>\
         <array><string>group.$(PRODUCT_BUNDLE_IDENTIFIER)</string></array>\
         </dict></plist>\n",
    )
    .unwrap();

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.ios_root = Some(ios_root.clone());
    opts.ios_app_dir = Some("MyApp".to_owned());
    opts.android = Some(false);
    opts.extra_contracts.push(dummy_contract());
    opts.extra_native_ios_dirs = vec![("ISTMO_SHARE".to_owned(), plugin_dir.clone())];
    emit_app_with(opts);

    let info = fs::read_to_string(app_dir.join("Info.plist")).unwrap();
    assert!(info.contains("NSPhotoLibraryAddUsageDescription"), "{info}");
    assert!(!info.contains("Ignored"), "app keys must win: {info}");
    assert!(app_dir.join("Info.plist.plugins.xml").is_file());

    let sidecar = fs::read_to_string(app_dir.join("MyApp.entitlements.plugins.xml")).unwrap();
    assert!(
        sidecar.contains("com.apple.security.application-groups"),
        "{sidecar}"
    );

    let yaml = fs::read_to_string(app_dir.join("istmo-plugins.yml")).unwrap();
    assert!(yaml.contains("\"*.fragment\""), "{yaml}");

    let _ = fs::remove_dir_all(&root);
}
