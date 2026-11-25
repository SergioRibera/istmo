//! The Kotlin runtime and this crate agree on every JNI entry and every
//! status code: a renamed `external fun`, a missing export or a drifted
//! code fails here instead of as an `UnsatisfiedLinkError` on a device.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use istmo_core::native_frame::{NATIVE_ABI_VERSION, SubmitStatus};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> Option<String> {
    std::fs::read_to_string(root().join(rel)).ok()
}

const KOTLIN: &str = "runtime/android/src/main/java/dev/istmo/runtime";

/// `external fun <name>` declared between `object <class>` and the
/// object's end (the next top-level `}`).
fn kotlin_externals(source: &str, class: &str) -> BTreeSet<String> {
    let start = source
        .find(&format!("object {class}"))
        .unwrap_or_else(|| panic!("object {class} not found"));
    let body = &source[start..];
    let end = body.find("\n}").unwrap_or(body.len());
    body[..end]
        .lines()
        .filter_map(|line| {
            let at = line.find("external fun ")? + "external fun ".len();
            let name = &line[at..];
            Some(name[..name.find('(')?].trim().to_owned())
        })
        .collect()
}

/// Every `#[unsafe(no_mangle)]` JNI function for `class` in `source`.
fn rust_exports(source: &str, class: &str) -> BTreeSet<String> {
    let prefix = format!("Java_dev_istmo_runtime_{class}_");
    source
        .split("#[unsafe(no_mangle)]")
        .skip(1)
        .filter_map(|item| {
            let at = item.find(&prefix)? + prefix.len();
            let name = &item[at..];
            let end = name.find(|c: char| !c.is_alphanumeric() && c != '_')?;
            Some(name[..end].to_owned())
        })
        .collect()
}

/// Names `pub use`d with the `Java_dev_istmo_runtime_<class>_` prefix.
fn reexports(source: &str, class: &str) -> BTreeSet<String> {
    let prefix = format!("Java_dev_istmo_runtime_{class}_");
    source
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '}')
        .filter_map(|token| token.strip_prefix(&prefix).map(ToOwned::to_owned))
        .collect()
}

#[test]
fn every_kotlin_external_has_a_rust_export_and_back() {
    let (Some(runtime_kt), Some(frames_kt)) = (
        read(&format!("{KOTLIN}/IstmoRuntime.kt")),
        read(&format!("{KOTLIN}/IstmoFrameSender.kt")),
    ) else {
        // Published crate: the Kotlin runtime is not in the tarball.
        return;
    };
    let jni = read("crates/android/src/jni_exports.rs").expect("jni_exports.rs");
    let frames = read("crates/android/src/frame_exports.rs").expect("frame_exports.rs");
    let lib = read("crates/android/src/lib.rs").expect("lib.rs");
    let entry = read("crates/android/src/entrypoint.rs").expect("entrypoint.rs");

    for (class, kotlin, rust) in [
        (
            "IstmoRuntime",
            kotlin_externals(&runtime_kt, "IstmoRuntime"),
            rust_exports(&jni, "IstmoRuntime"),
        ),
        (
            "IstmoFrames",
            kotlin_externals(&frames_kt, "IstmoFrames"),
            rust_exports(&frames, "IstmoFrames"),
        ),
    ] {
        assert!(!kotlin.is_empty(), "no externals found in {class}");
        // A Kotlin external without its export is an `UnsatisfiedLinkError`
        // on first call. `nativeSubmitCallDirect` is the one export
        // Kotlin does not declare yet (a fast path nothing calls).
        let missing: Vec<_> = kotlin.difference(&rust).collect();
        assert!(
            missing.is_empty(),
            "{class}: Kotlin externals with no Rust export: {missing:?}"
        );
        let unused: Vec<_> = rust.difference(&kotlin).collect();
        let allowed: &[&str] = if class == "IstmoRuntime" {
            &["nativeSubmitCallDirect"]
        } else {
            &[]
        };
        assert!(
            unused.iter().all(|name| allowed.contains(&name.as_str())),
            "{class}: Rust exports Kotlin does not declare: {unused:?}"
        );
        assert_eq!(reexports(&lib, class), rust, "{class}: lib.rs re-exports");
        assert_eq!(
            reexports(&entry, class),
            rust,
            "{class}: entrypoint.rs (what the app links)"
        );
    }
}

#[test]
fn kotlin_status_codes_and_abi_match_rust() {
    let Some(frames_kt) = read(&format!("{KOTLIN}/IstmoFrameSender.kt")) else {
        return;
    };
    let start = frames_kt
        .find("enum class IstmoSubmitStatus")
        .expect("status enum");
    let body = &frames_kt[start..start + frames_kt[start..].find(";\n").expect("enum entries end")];
    let kotlin: Vec<(String, i32)> = body
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let open = line.find('(')?;
            let name = &line[..open];
            if name.is_empty() || !name.chars().all(|c| c.is_ascii_uppercase() || c == '_') {
                return None;
            }
            let code = line[open + 1..line.find(')')?].parse().ok()?;
            Some((name.to_owned(), code))
        })
        .collect();
    let rust: Vec<(String, i32)> = SubmitStatus::ALL
        .iter()
        .map(|s| (s.name().to_owned(), s.code()))
        .collect();
    assert_eq!(
        kotlin, rust,
        "IstmoSubmitStatus (Kotlin) vs SubmitStatus (Rust)"
    );
    assert!(
        frames_kt.contains(&format!("const val ABI_VERSION = {NATIVE_ABI_VERSION}")),
        "IstmoFrames.ABI_VERSION must be {NATIVE_ABI_VERSION}"
    );
}

#[test]
fn the_ci_stub_declares_no_stale_natives() {
    let Some(stub) = read("runtime/android/ci/IstmoRuntimeStub.kt") else {
        return;
    };
    assert!(
        !stub.contains("fun native"),
        "IstmoFrameSender's natives live in `IstmoFrames`, compiled for real in CI"
    );
}
