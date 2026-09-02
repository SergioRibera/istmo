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

use istmo_build::{
    Arg, Contract, Method, MethodKind, ServiceContract, TypeRef, WorkerContract,
    generate_android_service, generate_android_worker, generate_kotlin, generate_swift,
};

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

fn permissions() -> Contract {
    Contract {
        plugin_id: "istmo.permissions".to_owned(),
        type_name: "Permissions".to_owned(),
        methods: vec![
            Method {
                name: "check".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "permission".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Named("PermissionStatus".to_owned()),
                error: None,
            },
            Method {
                name: "request".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "permissions".to_owned(),
                    ty: TypeRef::Vec(Box::new(TypeRef::String)),
                }],
                returns: TypeRef::Vec(Box::new(TypeRef::Named("PermissionOutcome".to_owned()))),
                error: None,
            },
            Method {
                name: "should_show_rationale".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "permission".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Bool,
                error: None,
            },
        ],
        init: None,
    }
}

fn activity_results() -> Contract {
    Contract {
        plugin_id: "istmo.activity_results".to_owned(),
        type_name: "ActivityResults".to_owned(),
        methods: vec![Method {
            name: "launch".to_owned(),
            kind: MethodKind::Unary,
            args: vec![Arg {
                name: "request".to_owned(),
                ty: TypeRef::Named("IntentRequest".to_owned()),
            }],
            returns: TypeRef::Named("ActivityResult".to_owned()),
            error: Some(TypeRef::Named("ActivityLaunchError".to_owned())),
        }],
        init: None,
    }
}

#[test]
fn permissions_kotlin_matches_golden() {
    assert_matches("permissions", "kt", &generate_kotlin(&permissions()));
}

#[test]
fn permissions_swift_matches_golden() {
    assert_matches("permissions", "swift", &generate_swift(&permissions()));
}

#[test]
fn activity_results_kotlin_matches_golden() {
    assert_matches(
        "activity_results",
        "kt",
        &generate_kotlin(&activity_results()),
    );
}

#[test]
fn activity_results_swift_matches_golden() {
    assert_matches(
        "activity_results",
        "swift",
        &generate_swift(&activity_results()),
    );
}

fn minimal_service() -> ServiceContract {
    ServiceContract::new(
        "myapp.sync",
        "SyncService",
        "com.example.myapp.sync",
        "myapp_sync",
    )
}

fn foreground_service() -> ServiceContract {
    let mut c = ServiceContract::new(
        "myapp.backup",
        "BackupService",
        "com.example.myapp.backup",
        "myapp_backup",
    );
    c.foreground_service_type = Some("dataSync".to_owned());
    c.exported = false;
    c.permission = Some("com.example.myapp.BACKUP".to_owned());
    c
}

#[test]
fn minimal_service_kotlin_matches_golden() {
    let out = generate_android_service(&minimal_service());
    assert_matches("service_minimal", "kt", &out.kotlin);
}

#[test]
fn minimal_service_manifest_matches_golden() {
    let out = generate_android_service(&minimal_service());
    assert_matches("service_minimal", "xml", &out.manifest_fragment);
}

#[test]
fn foreground_service_kotlin_matches_golden() {
    let out = generate_android_service(&foreground_service());
    assert_matches("service_foreground", "kt", &out.kotlin);
}

#[test]
fn foreground_service_manifest_matches_golden() {
    let out = generate_android_service(&foreground_service());
    assert_matches("service_foreground", "xml", &out.manifest_fragment);
}

fn backup_worker() -> WorkerContract {
    WorkerContract::new(
        "myapp.backup",
        "BackupWorker",
        "com.example.myapp.backup",
        "myapp_backup",
    )
}

#[test]
fn backup_worker_kotlin_matches_golden() {
    assert_matches("worker_backup", "kt", &generate_android_worker(&backup_worker()));
}
