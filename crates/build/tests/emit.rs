//! End-to-end runs of the `emit` entry points against scratch crates.

use std::fs;
use std::path::{Path, PathBuf};

use istmo_build::{AppOpts, emit_with};

fn scratch_root(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("istmo-emit-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn seed_android_app(root: &Path) -> PathBuf {
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.app\"\n}\n",
    )
    .unwrap();
    fs::create_dir_all(root.join("src")).unwrap();
    fs::write(
        root.join("src/lib.rs"),
        "istmo::runtime!(plugins: [NetworkStateClient]);\n",
    )
    .unwrap();
    android_root
}

#[test]
fn emit_with_reads_istmo_toml_from_the_given_root() {
    let root = scratch_root("custom-root");
    let android_root = seed_android_app(&root);
    fs::write(
        root.join("istmo.toml"),
        "[app]\nios = false\nandroid_permissions = [\"android.permission.CAMERA\"]\n",
    )
    .unwrap();
    assert_ne!(std::env::current_dir().unwrap(), root);

    emit_with(AppOpts {
        root: Some(root),
        lib_name: Some("app".into()),
        ..AppOpts::default()
    });

    // `[app] android_permissions` only reaches the sidecar when the
    // manifest under `root` (not the cwd) was read.
    let body = fs::read_to_string(permissions_sidecar(&android_root)).unwrap();
    assert!(body.contains("android.permission.CAMERA"), "{body}");
}

fn permissions_sidecar(android_root: &Path) -> PathBuf {
    android_root.join("app/src/main/istmo-permissions.xml")
}

#[test]
#[should_panic(expected = "istmo.toml")]
fn emit_with_fails_the_build_on_a_malformed_manifest() {
    let root = scratch_root("malformed");
    fs::write(root.join("istmo.toml"), "[plugin]\nid = \"bad id\"\n").unwrap();
    emit_with(AppOpts {
        root: Some(root),
        ..AppOpts::default()
    });
}
