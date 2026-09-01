//! Golden-file tests for the Kotlin and Swift generators.
//!
//! Each fixture builds a [`Contract`] by hand and compares the generator
//! output against a file at `tests/golden/{fixture}.{kt|swift}.expected`.
//!
//! Set `ISTMO_BUILD_UPDATE_GOLDEN=1` in the environment to overwrite the
//! expected files with the current output. Handy after intentional format
//! changes; never commit stale updates.

use std::fs;
use std::path::PathBuf;

use istmo_build::{Arg, Contract, Method, MethodKind, TypeRef, generate_kotlin, generate_swift};

fn golden_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("golden")
}

fn assert_matches(name: &str, extension: &str, actual: &str) {
    let path = golden_dir().join(format!("{name}.{extension}.expected"));
    if std::env::var("ISTMO_BUILD_UPDATE_GOLDEN").is_ok() {
        fs::write(&path, actual).expect("update golden");
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {}", path.display(), e));
    assert_eq!(
        actual, expected,
        "golden mismatch for {name}.{extension}\nRun `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-build` to refresh.",
    );
}

fn unary_only() -> Contract {
    Contract {
        plugin_id: "com.example.echo".to_owned(),
        type_name: "Echo".to_owned(),
        methods: vec![
            Method {
                name: "ping".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "msg".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::String,
                error: Some(TypeRef::Named("EchoError".to_owned())),
            },
            Method {
                name: "add".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "a".to_owned(),
                        ty: TypeRef::I32,
                    },
                    Arg {
                        name: "b".to_owned(),
                        ty: TypeRef::I32,
                    },
                ],
                returns: TypeRef::I32,
                error: None,
            },
        ],
        init: None,
    }
}

fn mixed_with_init() -> Contract {
    Contract {
        plugin_id: "com.example.watch".to_owned(),
        type_name: "Watch".to_owned(),
        methods: vec![
            Method {
                name: "watchLocation".to_owned(),
                kind: MethodKind::Stream,
                args: vec![Arg {
                    name: "accuracy".to_owned(),
                    ty: TypeRef::Named("Accuracy".to_owned()),
                }],
                returns: TypeRef::Named("Position".to_owned()),
                error: Some(TypeRef::Named("WatchError".to_owned())),
            },
            Method {
                name: "history".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "since".to_owned(),
                    ty: TypeRef::Option(Box::new(TypeRef::U64)),
                }],
                returns: TypeRef::Vec(Box::new(TypeRef::Named("Position".to_owned()))),
                error: None,
            },
            Method {
                name: "raw".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "packet".to_owned(),
                    ty: TypeRef::Bytes,
                }],
                returns: TypeRef::Bool,
                error: None,
            },
        ],
        init: Some(TypeRef::Named("WatchConfig".to_owned())),
    }
}

#[test]
fn unary_only_kotlin_matches_golden() {
    assert_matches("unary_only", "kt", &generate_kotlin(&unary_only()));
}

#[test]
fn unary_only_swift_matches_golden() {
    assert_matches("unary_only", "swift", &generate_swift(&unary_only()));
}

#[test]
fn mixed_with_init_kotlin_matches_golden() {
    assert_matches(
        "mixed_with_init",
        "kt",
        &generate_kotlin(&mixed_with_init()),
    );
}

#[test]
fn mixed_with_init_swift_matches_golden() {
    assert_matches(
        "mixed_with_init",
        "swift",
        &generate_swift(&mixed_with_init()),
    );
}
