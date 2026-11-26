//! The Kotlin runtime and this crate agree on every JNI entry, its
//! signature and every status code: a renamed or retyped `external fun`
//! or callback, a missing export or a drifted code fails here instead of
//! as an `UnsatisfiedLinkError`, a `NoSuchMethodError` or undefined
//! behaviour of an unchecked call on a device. The bench app's natives
//! and callbacks get the same checks.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use istmo_core::native_frame::{NATIVE_ABI_VERSION, SubmitStatus};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read(rel: &str) -> Option<String> {
    std::fs::read_to_string(root().join(rel)).ok()
}

/// A file of the repository outside this crate (the Kotlin sources, the
/// bench app). `None` only in the published crate, whose tarball has
/// none of them (`cargo package` adds `Cargo.toml.orig`); anywhere else
/// a missing file fails, so the checks never pass by not running.
fn repo_source(rel: &str) -> Option<String> {
    match std::fs::read_to_string(root().join(rel)) {
        Ok(source) => Some(source),
        Err(_)
            if Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("Cargo.toml.orig")
                .exists() =>
        {
            None
        }
        Err(err) => panic!("{rel}: {err}; the parity checks need the repository's sources"),
    }
}

const KOTLIN: &str = "runtime/android/src/main/java/dev/istmo/runtime";
const RUNTIME_PREFIX: &str = "Java_dev_istmo_runtime_";
const BENCH_KOTLIN: &str =
    "examples/bench-android/android/app/src/main/java/dev/istmo/bench/BenchNative.kt";
const BENCH_JNI: &str = "examples/bench-android/src/jni.rs";
const BENCH_PREFIX: &str = "Java_dev_istmo_bench_BenchNative_";

/// The body of `object <class>`, up to its end (the next top-level `}`).
fn kotlin_object<'a>(source: &'a str, class: &str) -> &'a str {
    let start = source
        .find(&format!("object {class}"))
        .unwrap_or_else(|| panic!("object {class} not found"));
    let body = &source[start..];
    &body[..body.find("\n}").unwrap_or(body.len())]
}

/// `external fun <name>` declared in `object <class>`.
fn kotlin_externals(source: &str, class: &str) -> BTreeSet<String> {
    kotlin_external_signatures(source, class)
        .into_keys()
        .collect()
}

/// Every `#[unsafe(no_mangle)]` JNI function whose symbol starts with
/// `prefix`, without it.
fn rust_exports(source: &str, prefix: &str) -> BTreeSet<String> {
    rust_export_signatures(source, prefix).into_keys().collect()
}

/// Names `pub use`d with the `Java_dev_istmo_runtime_<class>_` prefix.
fn reexports(source: &str, class: &str) -> BTreeSet<String> {
    let prefix = format!("{RUNTIME_PREFIX}{class}_");
    source
        .split(|c: char| c.is_whitespace() || c == ',' || c == '{' || c == '}')
        .filter_map(|token| token.strip_prefix(&prefix).map(ToOwned::to_owned))
        .collect()
}

#[test]
fn every_kotlin_external_has_a_rust_export_and_back() {
    let (Some(runtime_kt), Some(frames_kt)) = (
        repo_source(&format!("{KOTLIN}/IstmoRuntime.kt")),
        repo_source(&format!("{KOTLIN}/IstmoFrameSender.kt")),
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
            rust_exports(&jni, "Java_dev_istmo_runtime_IstmoRuntime_"),
        ),
        (
            "IstmoFrames",
            kotlin_externals(&frames_kt, "IstmoFrames"),
            rust_exports(&frames, "Java_dev_istmo_runtime_IstmoFrames_"),
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
fn every_bench_external_has_a_rust_export_and_back() {
    let (Some(bench_kt), Some(bench_jni)) = (repo_source(BENCH_KOTLIN), repo_source(BENCH_JNI))
    else {
        return;
    };
    let kotlin = kotlin_externals(&bench_kt, "BenchNative");
    let rust = rust_exports(&bench_jni, BENCH_PREFIX);
    assert!(!kotlin.is_empty(), "no externals found in BenchNative");
    assert_eq!(kotlin, rust, "BenchNative externals vs Rust exports");
}

#[test]
fn kotlin_status_codes_and_abi_match_rust() {
    let Some(frames_kt) = repo_source(&format!("{KOTLIN}/IstmoFrameSender.kt")) else {
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
fn every_callback_rust_resolves_is_a_jvm_static_on_the_runtime() {
    let Some(runtime_kt) = repo_source(&format!("{KOTLIN}/IstmoRuntime.kt")) else {
        return;
    };
    let callbacks = runtime_callbacks();
    assert!(callbacks.contains_key("onCallDirect"), "{callbacks:?}");
    for name in callbacks.keys() {
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
    let Some(stub) = repo_source("runtime/android/ci/IstmoRuntimeStub.kt") else {
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
    kotlin_object(source, class)
        .split("external fun ")
        .skip(1)
        .map(|decl| {
            let name = decl[..decl.find('(').expect("name")].trim().to_owned();
            (name, kotlin_signature(decl))
        })
        .collect()
}

/// One `#[unsafe(no_mangle)]` JNI export.
#[derive(Debug)]
struct Export {
    /// Parameter names and types after `env` and the caller.
    params: Vec<(String, String)>,
    /// `None` for no return value.
    returns: Option<String>,
    /// The function body.
    body: String,
}

/// Every JNI export whose symbol starts with `prefix`, by name without it.
fn rust_export_signatures(source: &str, prefix: &str) -> BTreeMap<String, Export> {
    source
        .split("#[unsafe(no_mangle)]")
        .skip(1)
        .filter_map(|item| {
            let at = item.find(prefix)? + prefix.len();
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
                .map(|param| {
                    let (name, ty) = param.split_once(": ").unwrap_or(("", param));
                    (name.trim().to_owned(), rust_type(ty))
                })
                .skip(2)
                .collect();
            let body_at = rest[close..].find('{').map_or(rest.len(), |at| close + at);
            let returns = rest[close..body_at]
                .split_once("->")
                .map(|(_, ty)| rust_type(ty));
            let body_end = rest[body_at..]
                .find("\n}")
                .map_or(rest.len(), |at| body_at + at);
            Some((
                name.to_owned(),
                Export {
                    params,
                    returns,
                    body: rest[body_at..body_end].to_owned(),
                },
            ))
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
        "LongArray" => "JLongArray",
        "Class<*>" => "JClass",
        "ByteBuffer" | "java.nio.ByteBuffer" => "JByteBuffer",
        other if other.contains('.') => "JObject",
        other => panic!("no JNI mapping for Kotlin parameter type {other}"),
    }
}

/// The JNI return types Rust may declare for a Kotlin return type (none
/// for `Unit`): the raw handle or its typed wrapper.
fn jni_return(kotlin: &str) -> &'static [&'static str] {
    match kotlin.trim_end_matches('?') {
        "Unit" => &[],
        "Long" => &["jlong"],
        "Int" => &["jint"],
        "Boolean" => &["jboolean"],
        "String" => &["jstring", "JString"],
        "ByteArray" => &["jbyteArray", "JByteArray"],
        "LongArray" => &["jlongArray", "JLongArray"],
        other if other.starts_with("Array<") => &["jobjectArray", "JObjectArray"],
        other => panic!("no JNI mapping for Kotlin return type {other}"),
    }
}

/// Every external of `kotlin` has the parameter and return types its
/// export declares, and a nullable object parameter is null-checked.
fn check_export_signatures(
    what: &str,
    kotlin: &BTreeMap<String, (Vec<String>, String)>,
    rust: &BTreeMap<String, Export>,
    helpers: &str,
) {
    assert!(!kotlin.is_empty(), "no externals found in {what}");
    for (name, (params, ret)) in kotlin {
        let export = rust.get(name).expect("export (checked above)");
        let types: Vec<_> = export.params.iter().map(|(_, ty)| ty.as_str()).collect();
        let expected: Vec<_> = params.iter().map(|ty| jni_param(ty)).collect();
        assert_eq!(
            types, expected,
            "{what}.{name}: Rust parameters vs Kotlin {params:?}"
        );
        let accepted = jni_return(ret);
        match export.returns.as_deref() {
            None => assert!(accepted.is_empty(), "{what}.{name}: Kotlin returns {ret}"),
            Some(ty) => assert!(
                accepted.contains(&ty),
                "{what}.{name}: Rust returns {ty}, Kotlin {ret}"
            ),
        }
        for ((param, _), kotlin_ty) in export.params.iter().zip(params) {
            if kotlin_ty.ends_with('?') && jni_param(kotlin_ty).starts_with('J') {
                assert!(
                    null_checked(&export.body, param, helpers),
                    "{what}.{name}: `{param}` is `{kotlin_ty}` in Kotlin; Rust must accept null"
                );
            }
        }
    }
}

/// `param` is tested with `is_null()` in `body`, or handed to a function
/// of `helpers` that tests its argument.
fn null_checked(body: &str, param: &str, helpers: &str) -> bool {
    if body.contains(&format!("{param}.is_null()")) {
        return true;
    }
    let by_ref = format!("&{param}");
    helpers.split("fn ").skip(1).any(|function| {
        let Some(name) = function.split(['(', '<']).next().filter(|name| {
            !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
        }) else {
            return false;
        };
        let end = function.find("\n}").unwrap_or(function.len());
        let call = format!("{name}(");
        let passes = body.match_indices(&call).any(|(at, _)| {
            top_level_items(balanced(&body[at + call.len() - 1..]))
                .iter()
                .any(|arg| arg == param || *arg == by_ref)
        });
        passes && function[..end].contains("is_null()")
    })
}

#[test]
fn every_jni_export_has_the_signature_kotlin_declares() {
    let (Some(runtime_kt), Some(frames_kt)) = (
        repo_source(&format!("{KOTLIN}/IstmoRuntime.kt")),
        repo_source(&format!("{KOTLIN}/IstmoFrameSender.kt")),
    ) else {
        return;
    };
    let jni = read("crates/android/src/jni_exports.rs").expect("jni_exports.rs");
    let frames = read("crates/android/src/frame_exports.rs").expect("frame_exports.rs");
    check_export_signatures(
        "IstmoRuntime",
        &kotlin_external_signatures(&runtime_kt, "IstmoRuntime"),
        &rust_export_signatures(&jni, "Java_dev_istmo_runtime_IstmoRuntime_"),
        &jni,
    );
    check_export_signatures(
        "IstmoFrames",
        &kotlin_external_signatures(&frames_kt, "IstmoFrames"),
        &rust_export_signatures(&frames, "Java_dev_istmo_runtime_IstmoFrames_"),
        &frames,
    );
}

#[test]
fn every_bench_export_has_the_signature_kotlin_declares() {
    let (Some(bench_kt), Some(bench_jni)) = (repo_source(BENCH_KOTLIN), repo_source(BENCH_JNI))
    else {
        return;
    };
    check_export_signatures(
        "BenchNative",
        &kotlin_external_signatures(&bench_kt, "BenchNative"),
        &rust_export_signatures(&bench_jni, BENCH_PREFIX),
        &bench_jni,
    );
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

/// The JVM descriptor of a Kotlin parameter list and return type.
fn kotlin_descriptor(params: &[String], ret: &str) -> String {
    format!(
        "({}){}",
        params.iter().map(|ty| descriptor(ty)).collect::<String>(),
        descriptor(ret)
    )
}

/// A static method Rust resolves by name and descriptor.
#[derive(Debug, Clone)]
struct Callback {
    name: String,
    descriptor: String,
}

/// Every `<field>: env.get_static_method_id(class, "<name>", "<descriptor>")`
/// or `let <field> = …` in `source`, by field.
fn rust_callback_fields(source: &str) -> BTreeMap<String, Callback> {
    let mut fields = BTreeMap::new();
    let mut rest = source;
    while let Some(at) = rest.find("get_static_method_id(") {
        let before = rest[..at].trim_end().trim_end_matches("env.").trim_end();
        let before = before.trim_end_matches([':', '=']).trim_end();
        let field: String = before
            .chars()
            .rev()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let call = &rest[at..];
        let mut literals = call[..call.find(")?").unwrap_or(call.len())]
            .split('"')
            .skip(1)
            .step_by(2);
        if let (Some(name), Some(descriptor)) = (literals.next(), literals.next()) {
            fields.insert(
                field,
                Callback {
                    name: name.to_owned(),
                    descriptor: descriptor.to_owned(),
                },
            );
        }
        rest = &call["get_static_method_id(".len()..];
    }
    fields
}

/// What Rust resolves from the runtime class, by Kotlin name.
fn runtime_callbacks() -> BTreeMap<String, String> {
    ["crates/android/src/pump.rs", "crates/android/src/direct.rs"]
        .into_iter()
        .flat_map(|file| rust_callback_fields(&read(file).expect(file)).into_values())
        .map(|callback| (callback.name, callback.descriptor))
        .collect()
}

/// The text of `@JvmStatic fun <name>(` onwards in `source`, from `(`.
fn kotlin_static_fun<'a>(source: &'a str, name: &str) -> Option<&'a str> {
    let declaration = format!("fun {name}(");
    source.split("@JvmStatic").skip(1).find_map(|item| {
        item.find(&declaration)
            .map(|at| &item[at + declaration.len() - 1..])
    })
}

#[test]
fn every_callback_rust_resolves_has_the_kotlin_signature() {
    let Some(runtime_kt) = repo_source(&format!("{KOTLIN}/IstmoRuntime.kt")) else {
        return;
    };
    let callbacks = runtime_callbacks();
    assert!(callbacks.contains_key("onCallDirect"), "{callbacks:?}");
    for (name, rust) in &callbacks {
        let at = kotlin_static_fun(&runtime_kt, name)
            .unwrap_or_else(|| panic!("IstmoRuntime.{name} is not a @JvmStatic fun"));
        let (params, ret) = kotlin_signature(at);
        assert_eq!(
            rust,
            &kotlin_descriptor(&params, &ret),
            "IstmoRuntime.{name}: descriptor Rust resolves"
        );
    }
}

#[test]
fn every_bench_callback_has_the_kotlin_signature() {
    let (Some(bench_kt), Some(bench_jni)) = (repo_source(BENCH_KOTLIN), repo_source(BENCH_JNI))
    else {
        return;
    };
    // The bench picks name and descriptor as a pair of literals, then
    // resolves them: `("kNoopBytes", "([B)V")`.
    let pairs: Vec<(&str, &str)> = bench_jni
        .split("(\"")
        .skip(1)
        .filter_map(|item| {
            let (name, rest) = item.split_once('"')?;
            let descriptor = rest.strip_prefix(", \"")?;
            let descriptor = &descriptor[..descriptor.find('"')?];
            descriptor.starts_with('(').then_some((name, descriptor))
        })
        .collect();
    assert!(
        pairs.iter().any(|(name, _)| *name == "kNoop"),
        "no callbacks found in {BENCH_JNI}: {pairs:?}"
    );
    for (name, rust) in pairs {
        let at = kotlin_static_fun(&bench_kt, name)
            .unwrap_or_else(|| panic!("BenchNative.{name} is not a @JvmStatic fun"));
        let (params, ret) = kotlin_signature(at);
        assert_eq!(
            rust,
            kotlin_descriptor(&params, &ret),
            "BenchNative.{name}: descriptor Rust resolves"
        );
    }
}

/// The text between the parentheses that open at the start of `text`,
/// with nested parentheses and brackets balanced.
fn balanced(text: &str) -> &str {
    let mut depth = 0usize;
    for (at, c) in text.char_indices() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => {
                depth -= 1;
                if depth == 0 {
                    return &text[1..at];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced call: {text}")
}

/// The top-level comma-separated items of an argument list.
fn top_level_items(list: &str) -> Vec<String> {
    let mut items = Vec::new();
    let mut depth = 0usize;
    let mut current = String::new();
    for c in list.chars() {
        match c {
            '(' | '[' | '<' => depth += 1,
            ')' | ']' | '>' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                items.push(current.trim().to_owned());
                current.clear();
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    if !current.trim().is_empty() {
        items.push(current.trim().to_owned());
    }
    items
}

/// The last path segment of an expression: `self.methods.on_event` → `on_event`.
fn last_segment(expr: &str) -> &str {
    expr.rsplit('.').next().unwrap_or(expr).trim()
}

/// One call of a resolved static method: which one, what it declares to
/// return, the arguments (`None` when passed as a variable) and the code
/// after it in the calling function.
#[derive(Debug)]
struct CallSite {
    field: String,
    return_type: String,
    args: Option<Vec<String>>,
    after: String,
}

/// The element expressions of `&[a, b]`; `None` for anything else.
fn arg_list(expr: &str) -> Option<Vec<String>> {
    let list = expr.strip_prefix('&')?.trim_start();
    list.starts_with('[')
        .then(|| top_level_items(balanced(list)))
}

/// Every `call_static_method_unchecked` in `source`, with the method it
/// calls. One made through a helper that takes the method as a parameter
/// (`call_static_void(env, methods.on_x, &[…])`) counts once per call of
/// the helper.
fn unchecked_calls(source: &str) -> Vec<CallSite> {
    let mut calls = Vec::new();
    for (at, _) in source.match_indices("call_static_method_unchecked(") {
        let open = at + "call_static_method_unchecked".len();
        let args = top_level_items(balanced(&source[open..]));
        let [_, method, return_type, call_args] = args.as_slice() else {
            panic!("unexpected call_static_method_unchecked arguments: {args:?}");
        };
        let fn_start = source[..at].rfind("fn ").expect("enclosing fn");
        let fn_end = source[at..]
            .find("\n    }\n")
            .map_or(source.len(), |end| at + end);
        let after = source[at..fn_end].to_owned();
        if method.contains('.') {
            calls.push(CallSite {
                field: last_segment(method).to_owned(),
                return_type: return_type.clone(),
                args: arg_list(call_args),
                after,
            });
            continue;
        }
        // The method is the helper's parameter: follow its callers.
        let helper = &source[fn_start + 3..];
        let helper_name = &helper[..helper.find(['(', '<']).expect("helper name")];
        let params: Vec<String> = top_level_items(balanced(&helper[helper_name.len()..]))
            .into_iter()
            .filter(|param| !param.contains("self"))
            .map(|param| param.split(':').next().unwrap_or("").trim().to_owned())
            .collect();
        let method_at = params
            .iter()
            .position(|param| param == method)
            .expect("method parameter");
        let args_at = params
            .iter()
            .position(|param| param == call_args.trim_start_matches('&'));
        let call = format!(".{helper_name}(");
        for (site, _) in source.match_indices(&call) {
            let open = site + call.len() - 1;
            let site_args = top_level_items(balanced(&source[open..]));
            calls.push(CallSite {
                field: last_segment(&site_args[method_at]).to_owned(),
                return_type: return_type.clone(),
                args: args_at.and_then(|i| arg_list(&site_args[i])),
                after: String::new(),
            });
        }
    }
    calls
}

/// The parameter and return descriptors of a JVM method descriptor.
fn split_descriptor(descriptor: &str) -> (Vec<String>, String) {
    let inner = &descriptor[1..descriptor.find(')').expect("descriptor")];
    let mut params = Vec::new();
    let mut chars = inner.chars().peekable();
    while chars.peek().is_some() {
        let mut ty = String::new();
        while chars.peek() == Some(&'[') {
            ty.push('[');
            chars.next();
        }
        match chars.next() {
            Some('L') => {
                ty.push('L');
                for c in chars.by_ref() {
                    ty.push(c);
                    if c == ';' {
                        break;
                    }
                }
            }
            Some(c) => ty.push(c),
            None => break,
        }
        params.push(ty);
    }
    (
        params,
        descriptor[descriptor.find(')').expect("descriptor") + 1..].to_owned(),
    )
}

/// Whether a `ReturnType` expression fits a JVM return descriptor.
fn return_type_fits(return_type: &str, descriptor: &str) -> bool {
    let primitive = |name: &str| return_type.contains(&format!("Primitive::{name}"));
    match descriptor.chars().next() {
        Some('V') => primitive("Void"),
        Some('J') => primitive("Long"),
        Some('I') => primitive("Int"),
        Some('Z') => primitive("Boolean"),
        Some('[') => return_type.contains("ReturnType::Object") || return_type.contains("Array"),
        Some('L') => return_type.contains("ReturnType::Object"),
        _ => false,
    }
}

/// Whether a `JValue` argument fits a JVM parameter descriptor; `true`
/// for an argument passed as a variable, whose kind is not visible here.
fn arg_fits(arg: &str, descriptor: &str) -> bool {
    let Some(kind) = arg
        .strip_prefix("JValue::")
        .and_then(|rest| rest.split('(').next())
    else {
        return true;
    };
    match kind {
        "Long" => descriptor == "J",
        "Int" => descriptor == "I",
        "Bool" => descriptor == "Z",
        "Object" => descriptor.starts_with(['L', '[']),
        _ => false,
    }
}

/// Every unchecked call of a resolved method declares the return type,
/// the argument count and kinds its descriptor has; a null argument goes
/// only to a nullable Kotlin parameter, and a nullable Kotlin result is
/// null-checked.
fn check_unchecked_calls(what: &str, source: &str, kotlin: &str) {
    let fields = rust_callback_fields(source);
    let calls = unchecked_calls(source);
    assert!(!calls.is_empty(), "{what}: no unchecked calls found");
    for call in &calls {
        let callback = fields
            .get(&call.field)
            .unwrap_or_else(|| panic!("{what}: `{}` is not a resolved method", call.field));
        let name = &callback.name;
        let (params, ret) = split_descriptor(&callback.descriptor);
        assert!(
            return_type_fits(&call.return_type, &ret),
            "{what}: {name} returns `{ret}` but is called as `{}`",
            call.return_type
        );
        let Some(at) = kotlin_static_fun(kotlin, name) else {
            panic!("{what}: {name} is not a @JvmStatic fun");
        };
        let (kotlin_params, kotlin_ret) = kotlin_signature(at);
        if let Some(args) = &call.args {
            assert_eq!(
                args.len(),
                params.len(),
                "{what}: {name} arguments {args:?}"
            );
            for ((arg, param), kotlin_ty) in args.iter().zip(&params).zip(&kotlin_params) {
                assert!(
                    arg_fits(arg, param),
                    "{what}: {name} gets `{arg}` for `{param}`"
                );
                assert!(
                    !arg.contains("null") || kotlin_ty.ends_with('?'),
                    "{what}: {name} gets `{arg}` for the non-null Kotlin `{kotlin_ty}`"
                );
            }
        }
        if kotlin_ret.ends_with('?') && kotlin_ret != "Unit" {
            assert!(
                call.after.contains("is_null()"),
                "{what}: {name} may return null (`{kotlin_ret}`); Rust must check"
            );
        }
    }
}

#[test]
fn every_unchecked_call_matches_what_it_resolved() {
    let Some(runtime_kt) = repo_source(&format!("{KOTLIN}/IstmoRuntime.kt")) else {
        return;
    };
    for file in ["crates/android/src/pump.rs", "crates/android/src/direct.rs"] {
        check_unchecked_calls(file, &read(file).expect(file), &runtime_kt);
    }
}

#[test]
fn the_classes_and_constructors_rust_resolves_exist() {
    let Some(handler_kt) = repo_source(&format!("{KOTLIN}/PluginHandler.kt")) else {
        return;
    };
    let jni = read("crates/android/src/jni_exports.rs").expect("jni_exports.rs");
    assert!(
        jni.contains("find_class(\"dev/istmo/runtime/PluginException\")"),
        "Rust resolves PluginException by this name"
    );
    assert!(handler_kt.starts_with("package dev.istmo.runtime\n"));
    let at = handler_kt
        .find("class PluginException")
        .expect("class PluginException");
    let (params, _) = kotlin_signature(&handler_kt[at + "class PluginException".len()..]);
    let constructor = kotlin_descriptor(&params, "Unit");
    assert!(
        jni.contains(&format!(
            "get_method_id(&class, \"<init>\", \"{constructor}\")"
        )),
        "PluginException's constructor is {constructor} in Kotlin"
    );
}
