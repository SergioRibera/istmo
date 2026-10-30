use std::fs;

use istmo_build::{AppOpts, Manifest, emit_app_with};

fn scratch_root(subdir: &str) -> std::path::PathBuf {
    let mut path = std::env::temp_dir();
    path.push(format!(
        "istmo-emit-perms-{}-{}",
        subdir,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn seed_android_project(root: &std::path::Path) -> std::path::PathBuf {
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.app\"\n}\n",
    )
    .unwrap();
    android_root
}

fn write_runtime_src(root: &std::path::Path, body: &str) {
    let src_dir = root.join("src");
    fs::create_dir_all(&src_dir).unwrap();
    fs::write(src_dir.join("lib.rs"), body).unwrap();
}

#[test]
fn wired_builtin_emits_matching_permission_sidecar() {
    let root = scratch_root("builtin-wired");
    let android_root = seed_android_project(&root);
    write_runtime_src(
        &root,
        "istmo::runtime!(plugins: [NetworkStateClient]);\n",
    );

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.lib_name = Some("app".into());

    emit_app_with(opts);

    let sidecar = android_root.join("app/src/main/istmo-permissions.xml");
    assert!(sidecar.is_file(), "sidecar missing at {}", sidecar.display());
    let body = fs::read_to_string(&sidecar).unwrap();
    assert!(
        body.contains("android.permission.ACCESS_NETWORK_STATE"),
        "wired NetworkStateClient should contribute ACCESS_NETWORK_STATE; got:\n{body}"
    );
}

#[test]
fn unwired_builtin_omits_permission() {
    let root = scratch_root("builtin-unwired");
    let android_root = seed_android_project(&root);
    write_runtime_src(&root, "istmo::runtime!(plugins: [SystemInfo]);\n");

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.lib_name = Some("app".into());

    emit_app_with(opts);

    let sidecar = android_root.join("app/src/main/istmo-permissions.xml");
    assert!(sidecar.is_file());
    let body = fs::read_to_string(&sidecar).unwrap();
    assert!(
        !body.contains("ACCESS_NETWORK_STATE"),
        "SystemInfo should not pull ACCESS_NETWORK_STATE; got:\n{body}"
    );
}

#[test]
fn app_level_permissions_land_in_sidecar() {
    let root = scratch_root("app-level");
    let android_root = seed_android_project(&root);
    write_runtime_src(&root, "fn main() {}\n");
    fs::write(
        root.join("istmo.toml"),
        r#"[app]
android_permissions = [
    "android.permission.INTERNET",
    { name = "android.permission.WRITE_EXTERNAL_STORAGE", max_sdk_version = 28 },
]
"#,
    )
    .unwrap();

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.lib_name = Some("app".into());

    emit_app_with(opts);

    let sidecar = android_root.join("app/src/main/istmo-permissions.xml");
    assert!(sidecar.is_file());
    let body = fs::read_to_string(&sidecar).unwrap();
    assert!(body.contains("android.permission.INTERNET"));
    assert!(body.contains("android.permission.WRITE_EXTERNAL_STORAGE"));
    assert!(body.contains("android:maxSdkVersion=\"28\""));
}

#[test]
fn dep_manifest_permission_merges_when_app_wires_it() {
    let root = scratch_root("dep-wired");
    let android_root = seed_android_project(&root);
    write_runtime_src(&root, "istmo::runtime!(plugins: [TransitiveClient]);\n");

    let dep = Manifest::parse(
        r#"
[plugin]
id = "ext.transitive"
client_type = "::some_other::TransitiveClient"
android_permissions = ["android.permission.BLUETOOTH"]
"#,
    )
    .unwrap();

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.lib_name = Some("app".into());
    opts.extra_manifests = vec![dep];

    emit_app_with(opts);

    let sidecar = android_root.join("app/src/main/istmo-permissions.xml");
    assert!(sidecar.is_file());
    let body = fs::read_to_string(&sidecar).unwrap();
    assert!(
        body.contains("android.permission.BLUETOOTH"),
        "dep-manifest plugin wired via runtime! should contribute its permission; got:\n{body}"
    );
}

#[test]
fn empty_wired_set_emits_empty_manifest() {
    let root = scratch_root("empty");
    let android_root = seed_android_project(&root);
    write_runtime_src(&root, "fn main() {}\n");

    let mut opts = AppOpts::default();
    opts.root = Some(root.clone());
    opts.android_root = Some(android_root.clone());
    opts.ios = Some(false);
    opts.lib_name = Some("app".into());

    emit_app_with(opts);

    let sidecar = android_root.join("app/src/main/istmo-permissions.xml");
    assert!(sidecar.is_file());
    let body = fs::read_to_string(&sidecar).unwrap();
    assert!(body.contains("<manifest"));
    assert!(
        !body.contains("<uses-permission"),
        "no plugins wired → zero uses-permission; got:\n{body}"
    );
}
