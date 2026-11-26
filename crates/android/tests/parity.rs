//! The Kotlin runtime and this crate agree on every JNI entry, its
//! signature and every status code: a renamed or retyped `external fun`
//! or callback, a missing export or a drifted code fails here instead of
//! as an `UnsatisfiedLinkError` or `NoSuchMethodError` on a device.

use std::collections::{BTreeMap, BTreeSet};
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

/// Names of the static Kotlin methods Rust resolves (`get_static_method_id`).
fn rust_callbacks(source: &str) -> BTreeSet<String> {
    source
        .split("get_static_method_id(")
        .skip(1)
        .filter_map(|call| {
            let open = call.find('"')? + 1;
            let len = call[open..].find('"')?;
            Some(call[open..open + len].to_owned())
        })
        .collect()
}

#[test]
fn every_callback_rust_resolves_is_a_jvm_static_on_the_runtime() {
    let Some(runtime_kt) = read(&format!("{KOTLIN}/IstmoRuntime.kt")) else {
        return;
    };
    let mut callbacks = rust_callbacks(&read("crates/android/src/pump.rs").expect("pump.rs"));
    callbacks.extend(rust_callbacks(
        &read("crates/android/src/direct.rs").expect("direct.rs"),
    ));
    assert!(callbacks.contains("onCallDirect"), "{callbacks:?}");
    for name in &callbacks {
        let declared = runtime_kt.split("@JvmStatic").skip(1).any(|item| {
            // Other annotations may sit between `@JvmStatic` and `fun`.
            item.lines()
                .map(str::trim)
                .find(|line| !line.is_empty() && !line.starts_with('@'))
                .is_some_and(|line| line.starts_with(&format!("fun {name}(")))
        });
        assert!(
            declared,
            "IstmoRuntime.{name} must be a @JvmStatic fun: Rust resolves it at start"
        );
    }
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

/// The parameter and return types of a Kotlin `fun`, from the text right
/// after its name (`(a: Long, b: String): ByteArray? { …`).
fn kotlin_signature(after_name: &str) -> (Vec<String>, String) {
    let open = after_name.find('(').expect("parameter list");
    let close = open + after_name[open..].find(')').expect("parameter list end");
    let params = after_name[open + 1..close]
        .split(',')
        .map(str::trim)
        .filter(|param| !param.is_empty())
        .map(|param| {
            let (_, ty) = param.split_once(':').expect("typed parameter");
            ty.split('=').next().unwrap_or(ty).trim().to_owned()
        })
        .collect();
    let rest = after_name[close + 1..].trim_start();
    let returns = rest.strip_prefix(':').map_or("Unit", |rest| {
        let end = rest.find(['{', '\n', '=']).unwrap_or(rest.len());
        rest[..end].trim()
    });
    (params, returns.to_owned())
}

/// `external fun` signatures declared in `object <class>`, by name.
fn kotlin_external_signatures(
    source: &str,
    class: &str,
) -> BTreeMap<String, (Vec<String>, String)> {
    let start = source.find(&format!("object {class}")).expect("object");
    let body = &source[start..];
    let body = &body[..body.find("\n}").unwrap_or(body.len())];
    body.split("external fun ")
        .skip(1)
        .map(|decl| {
            let name = decl[..decl.find('(').expect("name")].trim().to_owned();
            (name, kotlin_signature(decl))
        })
        .collect()
}

/// The Rust parameter types (after `env` and the caller) and return type
/// of every JNI export for `class`, by name.
fn rust_export_signatures(
    source: &str,
    class: &str,
) -> BTreeMap<String, (Vec<String>, Option<String>)> {
    let prefix = format!("Java_dev_istmo_runtime_{class}_");
    source
        .split("#[unsafe(no_mangle)]")
        .skip(1)
        .filter_map(|item| {
            let at = item.find(&prefix)? + prefix.len();
            let rest = &item[at..];
            let name = &rest[..rest.find(|c: char| !c.is_alphanumeric() && c != '_')?];
            // Skip the lifetime list, then take the parameter list.
            let open = rest.find("(\n").or_else(|| rest.find('('))?;
            let close = open
                + rest[open..]
                    .find("\n)")
                    .or_else(|| rest[open..].find(')'))?;
            let params = rest[open + 1..close]
                .split(",\n")
                .map(str::trim)
                .filter(|param| !param.is_empty())
                .map(|param| rust_type(param.split_once(": ").map_or(param, |(_, ty)| ty)))
                .skip(2)
                .collect();
            let after = &rest[close..rest[close..].find('{').map_or(rest.len(), |at| close + at)];
            let returns = after.split_once("->").map(|(_, ty)| rust_type(ty));
            Some((name.to_owned(), (params, returns)))
        })
        .collect()
}

/// `JByteArray<'local>,` → `JByteArray`; `jni::sys::jlongArray` → `jlongArray`.
fn rust_type(ty: &str) -> String {
    let ty = ty.trim().trim_end_matches(',');
    let ty = ty.split('<').next().unwrap_or(ty);
    ty.rsplit("::").next().unwrap_or(ty).trim().to_owned()
}

/// The JNI type Rust must declare for a Kotlin parameter type.
fn jni_param(kotlin: &str) -> &'static str {
    match kotlin.trim_end_matches('?') {
        "Long" => "jlong",
        "Int" => "jint",
        "Boolean" => "jboolean",
        "String" => "JString",
        "ByteArray" => "JByteArray",
        "IntArray" => "JIntArray",
        "Class<*>" => "JClass",
        "ByteBuffer" | "java.nio.ByteBuffer" => "JByteBuffer",
        other if other.contains('.') => "JObject",
        other => panic!("no JNI mapping for Kotlin parameter type {other}"),
    }
}

/// The JNI return type for a Kotlin return type (`None` for `Unit`).
fn jni_return(kotlin: &str) -> Option<&'static str> {
    Some(match kotlin.trim_end_matches('?') {
        "Unit" => return None,
        "Long" => "jlong",
        "Int" => "jint",
        "Boolean" => "jboolean",
        "ByteArray" => "jbyteArray",
        "LongArray" => "jlongArray",
        other if other.starts_with("Array<") => "jobjectArray",
        other => panic!("no JNI mapping for Kotlin return type {other}"),
    })
}

#[test]
fn every_jni_export_has_the_signature_kotlin_declares() {
    let (Some(runtime_kt), Some(frames_kt)) = (
        read(&format!("{KOTLIN}/IstmoRuntime.kt")),
        read(&format!("{KOTLIN}/IstmoFrameSender.kt")),
    ) else {
        return;
    };
    let jni = read("crates/android/src/jni_exports.rs").expect("jni_exports.rs");
    let frames = read("crates/android/src/frame_exports.rs").expect("frame_exports.rs");
    for (class, kotlin, rust) in [
        (
            "IstmoRuntime",
            kotlin_external_signatures(&runtime_kt, "IstmoRuntime"),
            rust_export_signatures(&jni, "IstmoRuntime"),
        ),
        (
            "IstmoFrames",
            kotlin_external_signatures(&frames_kt, "IstmoFrames"),
            rust_export_signatures(&frames, "IstmoFrames"),
        ),
    ] {
        assert!(!kotlin.is_empty(), "no externals found in {class}");
        for (name, (params, ret)) in &kotlin {
            let (rust_params, rust_ret) = rust.get(name).expect("export (checked above)");
            let expected: Vec<_> = params.iter().map(|ty| jni_param(ty)).collect();
            assert_eq!(
                rust_params, &expected,
                "{class}.{name}: Rust parameters vs Kotlin {params:?}"
            );
            assert_eq!(
                rust_ret.as_deref(),
                jni_return(ret),
                "{class}.{name}: Rust return type vs Kotlin {ret}"
            );
        }
    }
}

/// The JVM descriptor of a Kotlin type in a callback signature.
fn descriptor(kotlin: &str) -> &'static str {
    match kotlin.trim_end_matches('?') {
        "Unit" => "V",
        "Long" => "J",
        "Int" => "I",
        "Boolean" => "Z",
        "String" => "Ljava/lang/String;",
        "ByteArray" => "[B",
        "Array<ByteArray>" => "[[B",
        other => panic!("no JVM descriptor for Kotlin type {other}"),
    }
}

/// Every static method Rust resolves, with the descriptor it asks for.
fn rust_callback_descriptors(source: &str) -> BTreeMap<String, String> {
    source
        .split("get_static_method_id(")
        .skip(1)
        .filter_map(|call| {
            let mut literals = call.split('"').skip(1).step_by(2);
            Some((literals.next()?.to_owned(), literals.next()?.to_owned()))
        })
        .collect()
}

#[test]
fn every_callback_rust_resolves_has_the_kotlin_signature() {
    let Some(runtime_kt) = read(&format!("{KOTLIN}/IstmoRuntime.kt")) else {
        return;
    };
    let mut callbacks =
        rust_callback_descriptors(&read("crates/android/src/pump.rs").expect("pump.rs"));
    callbacks.extend(rust_callback_descriptors(
        &read("crates/android/src/direct.rs").expect("direct.rs"),
    ));
    assert!(callbacks.contains_key("onCallDirect"), "{callbacks:?}");
    for (name, rust) in &callbacks {
        let declaration = format!("fun {name}(");
        let at = runtime_kt
            .split("@JvmStatic")
            .skip(1)
            .find_map(|item| {
                item.find(&declaration)
                    .map(|at| &item[at + declaration.len() - 1..])
            })
            .unwrap_or_else(|| panic!("IstmoRuntime.{name} is not a @JvmStatic fun"));
        let (params, ret) = kotlin_signature(at);
        let kotlin = format!(
            "({}){}",
            params.iter().map(|ty| descriptor(ty)).collect::<String>(),
            descriptor(&ret)
        );
        assert_eq!(
            rust, &kotlin,
            "IstmoRuntime.{name}: descriptor Rust resolves"
        );
    }
}
