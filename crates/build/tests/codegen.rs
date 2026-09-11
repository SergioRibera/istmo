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
    Arg, BackgroundKind, ContinuousMode, Contract, DesktopAppContract, DesktopServiceContract,
    EnumDef, EnumVariant, Field, GradleCoord, GradleDep, GradleScope, IosBackgroundContract,
    IosEntitlements, Method, MethodKind, NativeDeps, RestartPolicy, ServiceContract, ServiceScope,
    StartType, StructDef, SwiftPackageDep, TypeDef, TypeRef, WorkerContract,
    generate_android_service, generate_android_worker, generate_desktop_entry,
    generate_ios_background, generate_kotlin, generate_kotlin_client, generate_kotlin_codecs,
    generate_kotlin_host, generate_kotlin_types, generate_launchd_plist, generate_rust_types,
    generate_swift, generate_swift_client, generate_swift_codecs, generate_swift_host,
    generate_swift_types, generate_systemd_unit, generate_windows_service, required_entitlements,
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
        types: vec![],
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
        types: vec![],
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
fn unary_only_kotlin_client_matches_golden() {
    assert_matches(
        "unary_only_client",
        "kt",
        &generate_kotlin_client(&unary_only()),
    );
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

#[test]
fn mixed_with_init_kotlin_client_matches_golden() {
    assert_matches(
        "mixed_with_init_client",
        "kt",
        &generate_kotlin_client(&mixed_with_init()),
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
        types: vec![],
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
        types: vec![],
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
        types: vec![],
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
        types: vec![],
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
        types: vec![],
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

#[test]
fn google_sign_in_kotlin_client_matches_golden() {
    assert_matches(
        "google_sign_in_client",
        "kt",
        &generate_kotlin_client(&google_sign_in()),
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

// ---- Type + codec generation (types + codecs for `Named` types) --------

fn permissions_with_types() -> Contract {
    let mut c = permissions();
    c.types = vec![
        TypeDef::Enum(EnumDef {
            name: "PermissionStatus".to_owned(),
            variants: vec![
                EnumVariant {
                    name: "Granted".to_owned(),
                    payload: vec![],
                },
                EnumVariant {
                    name: "Denied".to_owned(),
                    payload: vec![],
                },
                EnumVariant {
                    name: "PermanentlyDenied".to_owned(),
                    payload: vec![],
                },
                EnumVariant {
                    name: "NotDetermined".to_owned(),
                    payload: vec![],
                },
                EnumVariant {
                    name: "NotSupported".to_owned(),
                    payload: vec![],
                },
            ],
        }),
        TypeDef::Struct(StructDef {
            name: "PermissionOutcome".to_owned(),
            fields: vec![
                Field {
                    name: "permission".to_owned(),
                    ty: TypeRef::String,
                },
                Field {
                    name: "status".to_owned(),
                    ty: TypeRef::Named("PermissionStatus".to_owned()),
                },
            ],
        }),
    ];
    c
}

#[test]
fn permissions_kotlin_types_matches_golden() {
    assert_matches(
        "permissions_types",
        "kt",
        &generate_kotlin_types(&permissions_with_types()),
    );
}

#[test]
fn permissions_kotlin_codecs_matches_golden() {
    assert_matches(
        "permissions_codecs",
        "kt",
        &generate_kotlin_codecs(&permissions_with_types()),
    );
}

#[test]
fn permissions_swift_types_matches_golden() {
    assert_matches(
        "permissions_types",
        "swift",
        &generate_swift_types(&permissions_with_types()),
    );
}

#[test]
fn permissions_swift_codecs_matches_golden() {
    assert_matches(
        "permissions_codecs",
        "swift",
        &generate_swift_codecs(&permissions_with_types()),
    );
}

#[test]
fn permissions_rust_types_matches_golden() {
    assert_matches(
        "permissions_types",
        "rs",
        &generate_rust_types(&permissions_with_types()),
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

// ---- Desktop -----------------------------------------------------------

fn user_sync_service() -> DesktopServiceContract {
    let mut c = DesktopServiceContract::new(
        "myapp-sync",
        "MyApp background sync",
        "/usr/local/bin/myapp-sync",
    );
    "com.example.myapp.sync".clone_into(&mut c.label);
    c.restart = RestartPolicy::OnFailure;
    c.start_type = StartType::Auto;
    c.scope = ServiceScope::User;
    c.env = vec![
        ("RUST_LOG".to_owned(), "info".to_owned()),
        (
            "MYAPP_ENDPOINT".to_owned(),
            "https://api.example.com".to_owned(),
        ),
    ];
    c.args = vec![
        "--headless".to_owned(),
        "--config=/etc/myapp/sync.toml".to_owned(),
    ];
    c
}

fn system_daemon_service() -> DesktopServiceContract {
    let mut c = DesktopServiceContract::new(
        "myapp-indexer",
        "MyApp system-wide indexer",
        "/usr/bin/myapp-indexer",
    );
    "com.example.myapp.indexer".clone_into(&mut c.label);
    c.restart = RestartPolicy::Always;
    c.start_type = StartType::Auto;
    c.scope = ServiceScope::System;
    c.user = Some("myapp".to_owned());
    c.group = Some("myapp".to_owned());
    c.working_directory = Some("/var/lib/myapp".to_owned());
    c.after = vec!["network-online.target".to_owned()];
    c.stop_timeout_seconds = 60;
    c.windows_dependencies = vec!["Tcpip".to_owned(), "Dnscache".to_owned()];
    c.windows_display_name = Some("MyApp Indexer".to_owned());
    c
}

#[test]
fn systemd_user_unit_matches_golden() {
    assert_matches(
        "desktop_user_sync_systemd",
        "service",
        &generate_systemd_unit(&user_sync_service()),
    );
}

#[test]
fn systemd_system_unit_matches_golden() {
    assert_matches(
        "desktop_system_daemon_systemd",
        "service",
        &generate_systemd_unit(&system_daemon_service()),
    );
}

#[test]
fn launchd_user_plist_matches_golden() {
    assert_matches(
        "desktop_user_sync_launchd",
        "plist",
        &generate_launchd_plist(&user_sync_service()),
    );
}

#[test]
fn launchd_system_plist_matches_golden() {
    assert_matches(
        "desktop_system_daemon_launchd",
        "plist",
        &generate_launchd_plist(&system_daemon_service()),
    );
}

#[test]
fn windows_user_install_script_matches_golden() {
    let out = generate_windows_service(&user_sync_service());
    assert_matches(
        "desktop_user_sync_windows_install",
        "ps1",
        &out.install_script,
    );
}

#[test]
fn windows_user_uninstall_script_matches_golden() {
    let out = generate_windows_service(&user_sync_service());
    assert_matches(
        "desktop_user_sync_windows_uninstall",
        "ps1",
        &out.uninstall_script,
    );
}

#[test]
fn windows_system_install_script_matches_golden() {
    let out = generate_windows_service(&system_daemon_service());
    assert_matches(
        "desktop_system_daemon_windows_install",
        "ps1",
        &out.install_script,
    );
}

fn gui_app_entry() -> DesktopAppContract {
    let mut c = DesktopAppContract::new("MyApp", "/usr/bin/myapp %U");
    "Fast local-first workspace".clone_into(&mut c.comment);
    c.icon = Some("myapp".to_owned());
    c.categories = vec!["Utility".to_owned(), "Office".to_owned()];
    c.mime_types = vec!["x-scheme-handler/myapp".to_owned()];
    c.keywords = vec!["notes".to_owned(), "sync".to_owned()];
    c
}

#[test]
fn xdg_desktop_entry_matches_golden() {
    assert_matches(
        "desktop_app_myapp",
        "desktop",
        &generate_desktop_entry(&gui_app_entry()),
    );
}

// ---- Live-activity plugin ----------------------------------------------
//
// Mirrors the `#[istmo::plugin] trait LiveActivity` in
// `plugins/live-activity/src/lib.rs`. Hand-built here so the codegen
// tests do not depend on `istmo-live-activity` (which would create a
// circular dev-dependency edge — the plugin's own `build.rs` uses
// `istmo-build`). Golden fixtures pin the wire shape both sides speak.

fn live_activity() -> Contract {
    Contract {
        plugin_id: "istmo.live_activity".to_owned(),
        type_name: "LiveActivity".to_owned(),
        methods: vec![
            Method {
                name: "start".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "activity_type".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "attributes".to_owned(),
                        ty: TypeRef::Bytes,
                    },
                    Arg {
                        name: "initial_state".to_owned(),
                        ty: TypeRef::Bytes,
                    },
                    Arg {
                        name: "style".to_owned(),
                        ty: TypeRef::Named("ActivityStyle".to_owned()),
                    },
                    Arg {
                        name: "stale_after_seconds".to_owned(),
                        ty: TypeRef::Option(Box::new(TypeRef::U32)),
                    },
                    Arg {
                        name: "android_tier_hint".to_owned(),
                        ty: TypeRef::Option(Box::new(TypeRef::Named(
                            "AndroidTierHint".to_owned(),
                        ))),
                    },
                ],
                returns: TypeRef::Named("NativeHandleId".to_owned()),
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
            Method {
                name: "update".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "handle".to_owned(),
                        ty: TypeRef::Named("NativeHandleId".to_owned()),
                    },
                    Arg {
                        name: "state".to_owned(),
                        ty: TypeRef::Bytes,
                    },
                    Arg {
                        name: "alert".to_owned(),
                        ty: TypeRef::Option(Box::new(TypeRef::Named("AlertConfig".to_owned()))),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
            Method {
                name: "end".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "handle".to_owned(),
                        ty: TypeRef::Named("NativeHandleId".to_owned()),
                    },
                    Arg {
                        name: "final_state".to_owned(),
                        ty: TypeRef::Option(Box::new(TypeRef::Bytes)),
                    },
                    Arg {
                        name: "dismissal".to_owned(),
                        ty: TypeRef::Named("DismissalPolicy".to_owned()),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
            Method {
                name: "are_activities_enabled".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Bool,
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
            Method {
                name: "capabilities".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Named("PlatformCapabilities".to_owned()),
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
            Method {
                name: "restore_active".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Vec(Box::new(TypeRef::Named("RestoredActivity".to_owned()))),
                error: Some(TypeRef::Named("ActivityError".to_owned())),
            },
        ],
        init: None,
        types: vec![
            TypeDef::Enum(EnumDef {
                name: "ActivityStyle".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Standard".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Transient".to_owned(),
                        payload: vec![],
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "AlertSound".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Default".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Named".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                    EnumVariant {
                        name: "None".to_owned(),
                        payload: vec![],
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "AlertConfig".to_owned(),
                fields: vec![
                    Field {
                        name: "title".to_owned(),
                        ty: TypeRef::String,
                    },
                    Field {
                        name: "body".to_owned(),
                        ty: TypeRef::String,
                    },
                    Field {
                        name: "sound".to_owned(),
                        ty: TypeRef::Named("AlertSound".to_owned()),
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "DismissalPolicy".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Immediate".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Default".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "AfterSeconds".to_owned(),
                        payload: vec![TypeRef::U32],
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "AndroidTierHint".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "ForceCustom".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "PreferSystemTemplates".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "RequireLiveUpdate".to_owned(),
                        payload: vec![],
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "IosCapabilities".to_owned(),
                fields: vec![
                    Field {
                        name: "activity_kit_available".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "activities_enabled".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "dynamic_island".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "push_updates".to_owned(),
                        ty: TypeRef::Bool,
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "AndroidCapabilities".to_owned(),
                fields: vec![
                    Field {
                        name: "supports_custom".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "supports_progress_style".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "supports_live_update".to_owned(),
                        ty: TypeRef::Bool,
                    },
                    Field {
                        name: "notifications_enabled".to_owned(),
                        ty: TypeRef::Bool,
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "PlatformCapabilities".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Ios".to_owned(),
                        payload: vec![TypeRef::Named("IosCapabilities".to_owned())],
                    },
                    EnumVariant {
                        name: "Android".to_owned(),
                        payload: vec![TypeRef::Named("AndroidCapabilities".to_owned())],
                    },
                    EnumVariant {
                        name: "Unsupported".to_owned(),
                        payload: vec![],
                    },
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "RestoredActivity".to_owned(),
                fields: vec![
                    Field {
                        name: "handle".to_owned(),
                        ty: TypeRef::Named("NativeHandleId".to_owned()),
                    },
                    Field {
                        name: "activity_type".to_owned(),
                        ty: TypeRef::String,
                    },
                    Field {
                        name: "attributes".to_owned(),
                        ty: TypeRef::Bytes,
                    },
                    Field {
                        name: "state".to_owned(),
                        ty: TypeRef::Bytes,
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "ActivityError".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "NotSupported".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "Disabled".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "ExceededMaximum".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "HandleNotFound".to_owned(),
                        payload: vec![],
                    },
                    EnumVariant {
                        name: "UnknownActivityType".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                    EnumVariant {
                        name: "Decode".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                    EnumVariant {
                        name: "Backend".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                ],
            }),
        ],
    }
}

#[test]
fn live_activity_kotlin_host_matches_golden() {
    assert_matches(
        "live_activity_host",
        "kt",
        &generate_kotlin_host(&live_activity()),
    );
}

#[test]
fn live_activity_swift_host_matches_golden() {
    assert_matches(
        "live_activity_host",
        "swift",
        &generate_swift_host(&live_activity()),
    );
}

#[test]
fn live_activity_kotlin_types_matches_golden() {
    assert_matches(
        "live_activity_types",
        "kt",
        &generate_kotlin_types(&live_activity()),
    );
}

#[test]
fn live_activity_swift_types_matches_golden() {
    assert_matches(
        "live_activity_types",
        "swift",
        &generate_swift_types(&live_activity()),
    );
}

#[test]
fn live_activity_kotlin_codecs_matches_golden() {
    assert_matches(
        "live_activity_codecs",
        "kt",
        &generate_kotlin_codecs(&live_activity()),
    );
}

#[test]
fn live_activity_swift_codecs_matches_golden() {
    assert_matches(
        "live_activity_codecs",
        "swift",
        &generate_swift_codecs(&live_activity()),
    );
}

#[test]
fn live_activity_kotlin_client_matches_golden() {
    assert_matches(
        "live_activity_client",
        "kt",
        &generate_kotlin_client(&live_activity()),
    );
}

#[test]
fn live_activity_swift_client_matches_golden() {
    assert_matches(
        "live_activity_client",
        "swift",
        &generate_swift_client(&live_activity()),
    );
}
