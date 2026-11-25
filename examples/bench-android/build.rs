//! Generate the Kotlin side of the two bench plugins from
//! `src/contracts.rs`: the dispatcher Kotlin implements (`KotlinBench`)
//! and the client Kotlin calls (`RustBench`).

use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use istmo_build::{
    extract_contract, generate_kotlin_client, generate_kotlin_codecs, generate_kotlin_host,
    generate_kotlin_types,
};

const PACKAGE: &str = "dev.istmo.bench.gen";

/// Runtime types generated Kotlin may name unqualified (as `istmo-build`'s
/// app codegen imports them).
const RUNTIME_SYMBOLS: [&str; 11] = [
    "BackendException",
    "Bincode",
    "HandleReleaser",
    "IstmoBytesOut",
    "IstmoFrameSender",
    "IstmoGeneratedApi",
    "IstmoRuntime",
    "NativeHandleId",
    "PluginException",
    "PluginHandler",
    "PluginResult",
];

fn main() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let source = root.join("src/contracts.rs");
    let gen_dir = root.join("android/app/src/main/java/dev/istmo/bench/gen");

    let kotlin = extract_contract(&source, "KotlinBench").expect("KotlinBench contract");
    emit(
        &gen_dir,
        "KotlinBenchTypes.kt",
        &generate_kotlin_types(&kotlin),
    );
    emit(
        &gen_dir,
        "KotlinBenchCodecsImpl.kt",
        &generate_kotlin_codecs(&kotlin),
    );
    emit(
        &gen_dir,
        "KotlinBenchDispatcher.kt",
        &generate_kotlin_host(&kotlin),
    );

    let rust = extract_contract(&source, "RustBench").expect("RustBench contract");
    // The client of a contract whose methods name no message type needs
    // no codecs; its types are already declared by `KotlinBench`.
    emit(
        &gen_dir,
        "RustBenchClient.kt",
        &generate_kotlin_client(&rust),
    );

    println!("cargo::rerun-if-changed=src/contracts.rs");
    println!("cargo::rerun-if-changed=build.rs");
}

fn emit(dir: &Path, name: &str, body: &str) {
    let mut imports = String::new();
    for symbol in RUNTIME_SYMBOLS {
        if names(body, symbol) {
            let _ = writeln!(imports, "import dev.istmo.runtime.{symbol}");
        }
    }
    let contents = format!("package {PACKAGE}\n\n{imports}\n{body}");
    fs::create_dir_all(dir).expect("create the generated Kotlin directory");
    let path = dir.join(name);
    if fs::read_to_string(&path).is_ok_and(|old| old == contents) {
        return;
    }
    fs::write(&path, contents).expect("write generated Kotlin");
}

/// Whether a non-comment line of `body` uses `ident` as a whole word.
fn names(body: &str, ident: &str) -> bool {
    let is_ident = |c: char| c.is_ascii_alphanumeric() || c == '_';
    body.lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .any(|line| {
            line.match_indices(ident).any(|(at, _)| {
                !line[..at].chars().next_back().is_some_and(is_ident)
                    && !line[at + ident.len()..]
                        .chars()
                        .next()
                        .is_some_and(is_ident)
            })
        })
}
