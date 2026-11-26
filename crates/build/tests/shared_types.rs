//! Message types declared once in a shared crate (`istmo-media`) and used
//! by several plugins: extracted with each plugin's contract, generated
//! once per app.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use istmo_build::{
    AppOpts, Contract, SharedTypes, emit_app_with, extract_contract, extract_contract_with_shared,
    extract_shared_types, generate_kotlin_codecs, generate_kotlin_shared_types,
    generate_kotlin_types, generate_swift_codecs, generate_swift_shared_types,
    generate_swift_types,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "istmo-shared-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn write(dir: &Path, file: &str, source: &str) -> PathBuf {
    let path = dir.join(file);
    fs::write(&path, source).unwrap();
    path
}

const MEDIA: &str = r"
    use istmo::message;

    /// A standing permission.
    #[message]
    pub enum AccessStatus { Granted, Denied, NotDetermined }

    #[message]
    pub struct Extent { pub width: u32, pub height: u32 }
";

fn media(dir: &Path) -> SharedTypes {
    let path = write(dir, "media.rs", MEDIA);
    extract_shared_types(&path, "IstmoMedia", "dev.istmo.media").expect("shared types")
}

fn camera(dir: &Path) -> PathBuf {
    write(
        dir,
        "camera.rs",
        r#"
        use istmo::{message, plugin};
        pub use istmo_media::AccessStatus;

        #[message]
        pub struct Device { pub id: String }

        #[plugin(name = "test.camera", crate = "::istmo_core")]
        pub trait Camera {
            async fn access(&self) -> AccessStatus;
            async fn devices(&self) -> Vec<Device>;
        }
        "#,
    )
}

fn capture(dir: &Path) -> PathBuf {
    write(
        dir,
        "capture.rs",
        r#"
        use istmo::{message, plugin};
        use istmo_media::AccessStatus;

        #[message]
        pub struct AccessState { pub status: AccessStatus, pub scope: u8 }

        #[plugin(name = "test.capture", crate = "::istmo_core")]
        pub trait Capture {
            async fn access(&self) -> AccessState;
        }
        "#,
    )
}

#[test]
fn a_plugin_resolves_types_of_a_shared_crate() {
    let dir = scratch("extract");
    let media = media(&dir);
    assert_eq!(media.types.len(), 2);
    let camera = extract_contract_with_shared(camera(&dir), "Camera", std::slice::from_ref(&media))
        .expect("camera contract");
    assert_eq!(camera.shared, std::slice::from_ref(&media));
    assert!(camera.types.iter().all(|t| t.name() != "AccessStatus"));
    // Without the shared crate the type is unknown, as before.
    let err = extract_contract(camera_path(&dir), "Camera").expect_err("unknown type");
    assert!(err.to_string().contains("AccessStatus"), "{err}");
    // A plugin that uses nothing of the group does not carry it.
    let other = write(
        &dir,
        "other.rs",
        r#"
        use istmo::plugin;
        #[plugin(name = "test.other", crate = "::istmo_core")]
        pub trait Other { async fn ping(&self) -> u32; }
        "#,
    );
    let other = extract_contract_with_shared(other, "Other", &[media]).expect("other");
    assert!(other.shared.is_empty());
}

fn camera_path(dir: &Path) -> PathBuf {
    dir.join("camera.rs")
}

#[test]
fn a_type_declared_by_the_plugin_and_the_group_is_a_duplicate() {
    let dir = scratch("dup");
    let media = media(&dir);
    let plugin = write(
        &dir,
        "dup.rs",
        r#"
        use istmo::{message, plugin};
        #[message]
        pub enum AccessStatus { Granted }
        #[message]
        pub struct Probe { pub extent: Extent }
        #[plugin(name = "test.dup", crate = "::istmo_core")]
        pub trait Dup { async fn access(&self) -> AccessStatus; async fn probe(&self) -> Probe; }
        "#,
    );
    let err = extract_contract_with_shared(plugin, "Dup", &[media]).expect_err("duplicate");
    assert!(err.to_string().contains("AccessStatus"), "{err}");
}

#[test]
fn plugin_files_leave_shared_types_out_but_still_code_them() {
    let dir = scratch("codegen");
    let media = media(&dir);
    let camera =
        extract_contract_with_shared(camera(&dir), "Camera", std::slice::from_ref(&media)).unwrap();
    let kotlin_types = generate_kotlin_types(&camera);
    assert!(kotlin_types.contains("data class Device("));
    assert!(!kotlin_types.contains("AccessStatus"), "{kotlin_types}");
    let kotlin_codecs = generate_kotlin_codecs(&camera);
    assert!(kotlin_codecs.contains("override fun readAccessStatus("));
    assert!(kotlin_codecs.contains("override fun writeAccessStatus("));
    let swift_types = generate_swift_types(&camera);
    assert!(!swift_types.contains("enum AccessStatus"), "{swift_types}");
    assert!(generate_swift_codecs(&camera).contains("func readAccessStatus("));

    let kotlin_shared = generate_kotlin_shared_types(&media);
    assert!(kotlin_shared.contains("enum class AccessStatus {"));
    assert!(kotlin_shared.contains("data class Extent("));
    assert!(!kotlin_shared.contains("Codecs"), "declarations only");
    let swift_shared = generate_swift_shared_types(&media);
    assert!(swift_shared.contains("public enum AccessStatus"));
    assert!(swift_shared.contains("public struct Extent"));
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

/// Every file under `dir`, concatenated.
fn tree(dir: &Path) -> String {
    let mut out = String::new();
    let Ok(entries) = fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.push_str(&tree(&path));
        } else if let Ok(text) = fs::read_to_string(&path) {
            out.push_str(&text);
        }
    }
    out
}

fn contracts(dir: &Path) -> (Contract, Contract, SharedTypes) {
    let media = media(dir);
    let camera =
        extract_contract_with_shared(camera(dir), "Camera", std::slice::from_ref(&media)).unwrap();
    let capture =
        extract_contract_with_shared(capture(dir), "Capture", std::slice::from_ref(&media))
            .unwrap();
    (camera, capture, media)
}

#[test]
fn an_app_with_both_plugins_generates_each_shared_type_once() {
    let root = scratch("app");
    let (camera, capture, _) = contracts(&root);

    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    fs::write(
        android_root.join("app/build.gradle.kts"),
        "android {\n    namespace = \"com.example.app\"\n}\n",
    )
    .unwrap();
    let ios_root = root.join("ios");
    fs::create_dir_all(ios_root.join("MyApp")).unwrap();
    fs::create_dir_all(ios_root.join("MyApp.xcodeproj")).unwrap();

    emit_app_with(AppOpts {
        root: Some(root.clone()),
        android_root: Some(android_root.clone()),
        ios_root: Some(ios_root.clone()),
        extra_contracts: vec![camera, capture],
        ..AppOpts::default()
    });

    let java = android_root.join("app/src/main/java");
    let shared_kt = java.join("dev/istmo/media/IstmoMediaTypes.kt");
    let shared_kt = fs::read_to_string(&shared_kt).expect("one shared Kotlin file");
    assert!(shared_kt.starts_with("package dev.istmo.media\n"));
    let all_kotlin = tree(&java);
    assert_eq!(
        count(&all_kotlin, "enum class AccessStatus"),
        1,
        "declared once"
    );
    assert_eq!(count(&all_kotlin, "data class Extent("), 1);
    let app_pkg = java.join("com/example/app/gen");
    let codecs = fs::read_to_string(app_pkg.join("CameraCodecsImpl.kt")).expect("camera codecs");
    assert!(
        codecs.contains("import dev.istmo.media.AccessStatus\n"),
        "{codecs}"
    );
    let types = fs::read_to_string(app_pkg.join("CaptureTypes.kt")).expect("capture types");
    assert!(
        types.contains("import dev.istmo.media.AccessStatus\n"),
        "{types}"
    );
    assert!(
        !types.contains("import dev.istmo.media.Extent"),
        "only what it names"
    );

    let plugins = ios_root.join("MyApp/Plugins");
    let shared_swift = plugins.join("IstmoMedia/Generated/IstmoMediaTypes.swift");
    assert!(shared_swift.is_file(), "{}", shared_swift.display());
    let all_swift = tree(&plugins);
    assert_eq!(
        count(&all_swift, "public enum AccessStatus"),
        1,
        "declared once"
    );
    for plugin in ["Camera", "Capture"] {
        let codecs = fs::read_to_string(
            plugins.join(format!("{plugin}/Generated/{plugin}CodecsImpl.swift")),
        )
        .expect("codecs");
        assert!(
            codecs.contains("func readAccessStatus("),
            "{plugin} codes it"
        );
    }

    let _ = fs::remove_dir_all(&root);
}

#[test]
#[should_panic(expected = "differs between plugins")]
fn plugins_built_against_different_shared_types_fail_the_build() {
    let root = scratch("mismatch");
    let (camera, mut capture, mut media) = contracts(&root);
    if let Some(istmo_build::TypeDef::Enum(status)) = media.types.first_mut() {
        status.variants.pop();
    }
    capture.shared = vec![media];
    let ios_root = root.join("ios");
    fs::create_dir_all(ios_root.join("MyApp")).unwrap();
    fs::create_dir_all(ios_root.join("MyApp.xcodeproj")).unwrap();
    emit_app_with(AppOpts {
        root: Some(root),
        ios_root: Some(ios_root),
        android: Some(false),
        extra_contracts: vec![camera, capture],
        ..AppOpts::default()
    });
}
