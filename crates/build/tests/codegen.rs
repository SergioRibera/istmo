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
    Arg, BackgroundKind, ContinuousMode, Contract, GradleCoord, GradleDep, GradleScope,
    IosBackgroundContract, IosEntitlements, Method, MethodKind, NativeDeps, ServiceContract,
    SwiftPackageDep, TypeRef, WorkerContract, generate_android_service, generate_android_worker,
    generate_ios_background, generate_kotlin, generate_kotlin_host, generate_swift,
    generate_swift_client, generate_swift_host, required_entitlements,
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
    assert_matches(
        "worker_backup",
        "kt",
        &generate_android_worker(&backup_worker()),
    );
}

// ---- iOS ---------------------------------------------------------------

#[test]
fn unary_only_swift_client_matches_golden() {
    assert_matches(
        "unary_only_client",
        "swift",
        &generate_swift_client(&unary_only()),
    );
}

#[test]
fn mixed_with_init_swift_client_matches_golden() {
    assert_matches(
        "mixed_with_init_client",
        "swift",
        &generate_swift_client(&mixed_with_init()),
    );
}

fn refresh_background() -> IosBackgroundContract {
    IosBackgroundContract {
        plugin_id: "myapp.sync".to_owned(),
        class_name: "SyncScheduler".to_owned(),
        task_identifier: Some("com.example.myapp.sync".to_owned()),
        kind: BackgroundKind::Refresh {
            interval_minutes: 30,
        },
    }
}

fn processing_background() -> IosBackgroundContract {
    IosBackgroundContract {
        plugin_id: "myapp.reindex".to_owned(),
        class_name: "ReindexJob".to_owned(),
        task_identifier: Some("com.example.myapp.reindex".to_owned()),
        kind: BackgroundKind::Processing {
            requires_power: true,
            requires_network: false,
        },
    }
}

fn continuous_voip_background() -> IosBackgroundContract {
    IosBackgroundContract {
        plugin_id: "myapp.voip".to_owned(),
        class_name: "VoipHost".to_owned(),
        task_identifier: None,
        kind: BackgroundKind::Continuous(ContinuousMode::Voip),
    }
}

#[test]
fn ios_background_refresh_swift_matches_golden() {
    let out = generate_ios_background(&refresh_background());
    assert_matches("ios_background_refresh", "swift", &out.swift);
}

#[test]
fn ios_background_refresh_plist_matches_golden() {
    let out = generate_ios_background(&refresh_background());
    assert_matches("ios_background_refresh", "plist", &out.info_plist_fragment);
}

#[test]
fn ios_background_processing_swift_matches_golden() {
    let out = generate_ios_background(&processing_background());
    assert_matches("ios_background_processing", "swift", &out.swift);
}

#[test]
fn ios_background_processing_plist_matches_golden() {
    let out = generate_ios_background(&processing_background());
    assert_matches(
        "ios_background_processing",
        "plist",
        &out.info_plist_fragment,
    );
}

#[test]
fn ios_background_voip_swift_matches_golden() {
    let out = generate_ios_background(&continuous_voip_background());
    assert_matches("ios_background_voip", "swift", &out.swift);
}

#[test]
fn ios_background_voip_plist_matches_golden() {
    let out = generate_ios_background(&continuous_voip_background());
    assert_matches("ios_background_voip", "plist", &out.info_plist_fragment);
}

#[test]
fn ios_entitlements_voip_matches_golden() {
    let ent = required_entitlements(&continuous_voip_background());
    assert!(
        !ent.is_empty(),
        "voip mode should require pushkit entitlement"
    );
    assert_matches("ios_entitlements_voip", "plist", &ent.render());
}

fn admob() -> Contract {
    Contract {
        plugin_id: "istmo.admob".to_owned(),
        type_name: "AdMob".to_owned(),
        methods: vec![
            Method {
                name: "load_interstitial".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad_unit_id".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Named("NativeHandleId".to_owned()),
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "show_interstitial".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad".to_owned(),
                    ty: TypeRef::Named("NativeHandleId".to_owned()),
                }],
                returns: TypeRef::Named("InterstitialOutcome".to_owned()),
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "load_rewarded".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad_unit_id".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Named("NativeHandleId".to_owned()),
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "show_rewarded".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad".to_owned(),
                    ty: TypeRef::Named("NativeHandleId".to_owned()),
                }],
                returns: TypeRef::Named("RewardedOutcome".to_owned()),
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "show_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "request".to_owned(),
                    ty: TypeRef::Named("BannerRequest".to_owned()),
                }],
                returns: TypeRef::Named("NativeHandleId".to_owned()),
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "update_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "banner".to_owned(),
                        ty: TypeRef::Named("NativeHandleId".to_owned()),
                    },
                    Arg {
                        name: "rect".to_owned(),
                        ty: TypeRef::Named("BannerRect".to_owned()),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
            Method {
                name: "hide_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "banner".to_owned(),
                    ty: TypeRef::Named("NativeHandleId".to_owned()),
                }],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("AdError".to_owned())),
            },
        ],
        init: Some(TypeRef::Named("AdMobConfig".to_owned())),
    }
}

#[test]
fn admob_kotlin_matches_golden() {
    assert_matches("admob", "kt", &generate_kotlin(&admob()));
}

#[test]
fn admob_swift_matches_golden() {
    assert_matches("admob", "swift", &generate_swift(&admob()));
}

fn google_sign_in() -> Contract {
    Contract {
        plugin_id: "istmo.google_sign_in".to_owned(),
        type_name: "SignIn".to_owned(),
        methods: vec![
            Method {
                name: "sign_in".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "mode".to_owned(),
                    ty: TypeRef::Named("SignInMode".to_owned()),
                }],
                returns: TypeRef::Named("SignInAccount".to_owned()),
                error: Some(TypeRef::Named("SignInError".to_owned())),
            },
            Method {
                name: "silent_sign_in".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Option(Box::new(TypeRef::Named("SignInAccount".to_owned()))),
                error: Some(TypeRef::Named("SignInError".to_owned())),
            },
            Method {
                name: "refresh".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "credential".to_owned(),
                    ty: TypeRef::Named("NativeHandleId".to_owned()),
                }],
                returns: TypeRef::Named("SignInAccount".to_owned()),
                error: Some(TypeRef::Named("SignInError".to_owned())),
            },
            Method {
                name: "sign_out".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("SignInError".to_owned())),
            },
            Method {
                name: "revoke".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("SignInError".to_owned())),
            },
        ],
        init: Some(TypeRef::Named("SignInConfig".to_owned())),
    }
}

fn notifications() -> Contract {
    Contract {
        plugin_id: "istmo.notifications".to_owned(),
        type_name: "Notifications".to_owned(),
        methods: vec![
            Method {
                name: "is_authorized".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Bool,
                error: None,
            },
            Method {
                name: "request_authorization".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Bool,
                error: None,
            },
            Method {
                name: "schedule".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "request".to_owned(),
                    ty: TypeRef::Named("NotificationRequest".to_owned()),
                }],
                returns: TypeRef::Named("NotificationHandle".to_owned()),
                error: Some(TypeRef::Named("NotificationError".to_owned())),
            },
            Method {
                name: "cancel".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "id".to_owned(),
                    ty: TypeRef::U32,
                }],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("NotificationError".to_owned())),
            },
        ],
        init: None,
    }
}

#[test]
fn notifications_kotlin_matches_golden() {
    assert_matches("notifications", "kt", &generate_kotlin(&notifications()));
}

#[test]
fn notifications_swift_matches_golden() {
    assert_matches("notifications", "swift", &generate_swift(&notifications()));
}

#[test]
fn google_sign_in_kotlin_matches_golden() {
    assert_matches("google_sign_in", "kt", &generate_kotlin(&google_sign_in()));
}

#[test]
fn google_sign_in_swift_matches_golden() {
    assert_matches(
        "google_sign_in",
        "swift",
        &generate_swift(&google_sign_in()),
    );
}

#[test]
fn google_sign_in_swift_client_matches_golden() {
    assert_matches(
        "google_sign_in_client",
        "swift",
        &generate_swift_client(&google_sign_in()),
    );
}

fn google_sign_in_deps() -> NativeDeps {
    let mut d = NativeDeps::new();
    d.add_gradle(&GradleDep::new(
        GradleScope::Implementation,
        GradleCoord::new("androidx.credentials", "credentials", "1.3.0"),
    ))
    .add_gradle(&GradleDep::new(
        GradleScope::Implementation,
        GradleCoord::new(
            "androidx.credentials",
            "credentials-play-services-auth",
            "1.3.0",
        ),
    ))
    .add_gradle(&GradleDep::new(
        GradleScope::Implementation,
        GradleCoord::new(
            "com.google.android.libraries.identity.googleid",
            "googleid",
            "1.1.1",
        ),
    ))
    .add_swift_package(&SwiftPackageDep {
        url: "https://github.com/google/GoogleSignIn-iOS.git".to_owned(),
        product: "GoogleSignIn".to_owned(),
        from_version: "7.1.0".to_owned(),
    });
    d
}

#[test]
fn google_sign_in_deps_gradle_matches_golden() {
    assert_matches(
        "google_sign_in_deps",
        "gradle.kts",
        &google_sign_in_deps().render_gradle(),
    );
}

#[test]
fn native_deps_merge_prefers_highest_version() {
    let mut merged = google_sign_in_deps();
    let mut other = NativeDeps::new();
    other.add_gradle(&GradleDep::new(
        GradleScope::Implementation,
        GradleCoord::new("androidx.credentials", "credentials", "1.3.2"),
    ));
    merged.merge(other);
    let entries: Vec<_> = merged.gradle_entries().collect();
    let creds = entries
        .iter()
        .find(|e| e.coord.artifact == "credentials")
        .expect("credentials entry");
    assert_eq!(creds.coord.version, "1.3.2");
    assert!(
        merged
            .conflicts()
            .iter()
            .any(|c| c.key.artifact == "credentials" && c.picked == "1.3.2"),
        "conflict must be surfaced",
    );
}

// ---- Host dispatchers (Kotlin + Swift native-hosted plugin surface) -----

#[test]
fn permissions_kotlin_host_matches_golden() {
    assert_matches(
        "permissions_host",
        "kt",
        &generate_kotlin_host(&permissions()),
    );
}

#[test]
fn permissions_swift_host_matches_golden() {
    assert_matches(
        "permissions_host",
        "swift",
        &generate_swift_host(&permissions()),
    );
}

#[test]
fn notifications_kotlin_host_matches_golden() {
    assert_matches(
        "notifications_host",
        "kt",
        &generate_kotlin_host(&notifications()),
    );
}

#[test]
fn notifications_swift_host_matches_golden() {
    assert_matches(
        "notifications_host",
        "swift",
        &generate_swift_host(&notifications()),
    );
}

#[test]
fn google_sign_in_kotlin_host_matches_golden() {
    assert_matches(
        "google_sign_in_host",
        "kt",
        &generate_kotlin_host(&google_sign_in()),
    );
}

#[test]
fn google_sign_in_swift_host_matches_golden() {
    assert_matches(
        "google_sign_in_host",
        "swift",
        &generate_swift_host(&google_sign_in()),
    );
}

#[test]
fn admob_kotlin_host_matches_golden() {
    assert_matches("admob_host", "kt", &generate_kotlin_host(&admob()));
}

#[test]
fn admob_swift_host_matches_golden() {
    assert_matches("admob_host", "swift", &generate_swift_host(&admob()));
}

#[test]
fn ios_entitlements_merge_render() {
    let mut ent = IosEntitlements::new();
    ent.add_bool("com.apple.developer.healthkit", true)
        .add_string_array(
            "keychain-access-groups",
            ["group.com.example.app", "group.com.example.shared"],
        );
    ent.merge(required_entitlements(&continuous_voip_background()));
    assert_matches("ios_entitlements_merged", "plist", &ent.render());
}
