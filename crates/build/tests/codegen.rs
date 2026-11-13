use std::fs;
use std::path::PathBuf;

use istmo_build::{
    Arg, BackgroundKind, ContinuousMode, Contract, DesktopAppContract, DesktopServiceContract,
    EnumDef, EnumVariant, Field, GradleCoord, GradleDep, GradleScope, IosBackgroundContract,
    IosEntitlements, Method, MethodKind, NativeDeps, RegistryEntry, RestartPolicy, ServiceContract,
    ServiceScope, StartType, StructDef, SwiftPackageDep, TypeDef, TypeRef, WorkerContract,
    generate_android_service, generate_android_worker, generate_desktop_entry,
    generate_ios_background, generate_kotlin, generate_kotlin_client, generate_kotlin_codecs,
    generate_kotlin_host, generate_kotlin_plugin_registry, generate_kotlin_types,
    generate_launchd_plist, generate_rust_types, generate_swift, generate_swift_client,
    generate_swift_codecs, generate_swift_host, generate_swift_plugin_registry,
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
                        ty: TypeRef::Option(Box::new(TypeRef::Named("AndroidTierHint".to_owned()))),
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

fn multi_payload_contract() -> Contract {
    Contract {
        plugin_id: "com.example.multi".to_owned(),
        type_name: "Multi".to_owned(),
        methods: vec![],
        init: None,
        types: vec![TypeDef::Enum(EnumDef {
            name: "Frame".to_owned(),
            variants: vec![
                EnumVariant {
                    name: "Empty".to_owned(),
                    payload: vec![],
                },
                EnumVariant {
                    name: "One".to_owned(),
                    payload: vec![TypeRef::U32],
                },
                EnumVariant {
                    name: "Three".to_owned(),
                    payload: vec![
                        TypeRef::U32,
                        TypeRef::String,
                        TypeRef::Vec(Box::new(TypeRef::Bool)),
                    ],
                },
            ],
        })],
    }
}

#[test]
fn multi_payload_kotlin_types_emit_positional_fields() {
    let out = generate_kotlin_types(&multi_payload_contract());
    assert!(
        out.contains("object Empty : Frame()"),
        "unit variant broken: {out}",
    );
    assert!(
        out.contains("data class One(val value: UInt) : Frame()"),
        "single-payload variant broken: {out}",
    );
    assert!(
        out.contains(
            "data class Three(val f0: UInt, val f1: String, val f2: List<Boolean>) : Frame()"
        ),
        "multi-payload variant broken: {out}",
    );
}

#[test]
fn multi_payload_kotlin_codecs_read_write_all_fields() {
    let out = generate_kotlin_codecs(&multi_payload_contract());
    assert!(out.contains("val d_f0"), "reads first field: {out}");
    assert!(out.contains("val d_f1"), "reads second field: {out}");
    assert!(out.contains("val d_f2"), "reads third field: {out}");
    assert!(
        out.contains("Frame.Three(f0, f1, f2)"),
        "reconstructs multi-payload: {out}",
    );
    assert!(
        out.contains("is Frame.Three ->"),
        "write branch present: {out}",
    );
    assert!(out.contains("value.f0"), "writes first field: {out}");
    assert!(out.contains("value.f1"), "writes second field: {out}");
    assert!(out.contains("value.f2"), "writes third field: {out}");
}

#[test]
fn multi_payload_swift_types_emit_positional_associated_values() {
    let out = generate_swift_types(&multi_payload_contract());
    assert!(out.contains("case empty"), "unit variant: {out}");
    assert!(out.contains("case one(UInt32)"), "single-payload: {out}");
    assert!(
        out.contains("case three(UInt32, String, [Bool])"),
        "multi-payload: {out}",
    );
}

#[test]
fn multi_payload_swift_codecs_read_write_all_fields() {
    let out = generate_swift_codecs(&multi_payload_contract());
    assert!(out.contains("let f0 ="), "reads first field: {out}");
    assert!(out.contains("let f1 ="), "reads second field: {out}");
    assert!(out.contains("let f2 ="), "reads third field: {out}");
    assert!(
        out.contains("return .three(f0, f1, f2)"),
        "reconstructs multi-payload: {out}",
    );
    assert!(
        out.contains("case .three(let f0, let f1, let f2):"),
        "write pattern binds all fields: {out}",
    );
}

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}

fn boxed_vec(inner: TypeRef) -> TypeRef {
    TypeRef::Vec(Box::new(inner))
}

fn boxed_opt(inner: TypeRef) -> TypeRef {
    TypeRef::Option(Box::new(inner))
}

fn field(name: &str, ty: TypeRef) -> Field {
    Field {
        name: name.to_owned(),
        ty,
    }
}

fn arg(name: &str, ty: TypeRef) -> Arg {
    Arg {
        name: name.to_owned(),
        ty,
    }
}

fn variant(name: &str, payload: Vec<TypeRef>) -> EnumVariant {
    EnumVariant {
        name: name.to_owned(),
        payload,
    }
}

fn coverage_methods() -> Vec<Method> {
    vec![
        Method {
            name: "object".to_owned(),
            kind: MethodKind::Unary,
            args: vec![arg("in", TypeRef::U8), arg("val", TypeRef::I8)],
            returns: named("Everything"),
            error: Some(named("FirstError")),
        },
        Method {
            name: "round_trip".to_owned(),
            kind: MethodKind::Unary,
            args: vec![
                arg("value", named("Everything")),
                arg("shape", named("Shape")),
            ],
            returns: boxed_vec(boxed_opt(boxed_vec(TypeRef::String))),
            error: Some(named("SecondError")),
        },
        Method {
            name: "send".to_owned(),
            kind: MethodKind::Unary,
            args: vec![
                arg("payload", TypeRef::Bytes),
                arg("cursor", TypeRef::U16),
                arg("c", TypeRef::I16),
            ],
            returns: TypeRef::Unit,
            error: Some(named("FirstError")),
        },
        Method {
            name: "watch".to_owned(),
            kind: MethodKind::Stream,
            args: vec![arg("mode", named("Mode"))],
            returns: boxed_opt(TypeRef::Bytes),
            error: None,
        },
        Method {
            name: "follow".to_owned(),
            kind: MethodKind::Stream,
            args: vec![arg("next", TypeRef::U32)],
            returns: named("Shape"),
            error: Some(named("SecondError")),
        },
        Method {
            name: "default".to_owned(),
            kind: MethodKind::Unary,
            args: vec![],
            returns: TypeRef::Unit,
            error: None,
        },
        Method {
            name: "pulse".to_owned(),
            kind: MethodKind::Stream,
            args: vec![arg("gap", TypeRef::Unit), arg("hz", TypeRef::U32)],
            returns: TypeRef::Unit,
            error: None,
        },
    ]
}

fn coverage_types() -> Vec<TypeDef> {
    vec![
        TypeDef::Struct(StructDef {
            name: "Everything".to_owned(),
            fields: vec![
                field("unit", TypeRef::Unit),
                field("flag", TypeRef::Bool),
                field("byte", TypeRef::U8),
                field("signedByte", TypeRef::I8),
                field("short", TypeRef::U16),
                field("signedShort", TypeRef::I16),
                field("word", TypeRef::U32),
                field("signedWord", TypeRef::I32),
                field("long", TypeRef::U64),
                field("signedLong", TypeRef::I64),
                field("single", TypeRef::F32),
                field("double", TypeRef::F64),
                field("text", TypeRef::String),
                field("blob", TypeRef::Bytes),
                field("nested", boxed_vec(boxed_opt(boxed_vec(TypeRef::String)))),
                field("maybeBlob", boxed_opt(TypeRef::Bytes)),
                field("grid", boxed_vec(boxed_vec(TypeRef::U32))),
                field("shapes", boxed_opt(boxed_vec(named("Shape")))),
                field("handle", named("NativeHandleId")),
                field("object", TypeRef::String),
                field("default", TypeRef::Bool),
                field("where", boxed_opt(TypeRef::I64)),
                field("cursor", TypeRef::U8),
                field("c", TypeRef::U8),
            ],
        }),
        TypeDef::Enum(EnumDef {
            name: "Shape".to_owned(),
            variants: vec![
                variant("Default", vec![]),
                variant("Circle", vec![TypeRef::F64]),
                variant(
                    "Path",
                    vec![boxed_vec(TypeRef::I32), boxed_opt(TypeRef::String)],
                ),
            ],
        }),
        TypeDef::Enum(EnumDef {
            name: "Mode".to_owned(),
            variants: vec![variant("In", vec![]), variant("Out", vec![])],
        }),
        TypeDef::Enum(EnumDef {
            name: "FirstError".to_owned(),
            variants: vec![
                variant("Denied", vec![]),
                variant("Failed", vec![TypeRef::String]),
            ],
        }),
        TypeDef::Struct(StructDef {
            name: "SecondError".to_owned(),
            fields: vec![field("code", TypeRef::U32)],
        }),
    ]
}

/// Every `TypeRef`, three-level nesting, Kotlin/Swift keywords as type,
/// field, argument and method names, a hosted stream, names that clash
/// with generated locals and two distinct domain errors.
fn coverage() -> Contract {
    Contract {
        plugin_id: "com.example.\"cov$\"".to_owned(),
        type_name: "Coverage".to_owned(),
        methods: coverage_methods(),
        init: None,
        types: coverage_types(),
    }
}

#[test]
fn coverage_contract_is_valid() {
    assert_eq!(coverage().validate(), Ok(()));
}

#[test]
fn coverage_kotlin_matches_golden() {
    let c = coverage();
    assert_matches("coverage_types", "kt", &generate_kotlin_types(&c));
    assert_matches("coverage_codecs", "kt", &generate_kotlin_codecs(&c));
    assert_matches("coverage_host", "kt", &generate_kotlin_host(&c));
    assert_matches("coverage_client", "kt", &generate_kotlin_client(&c));
    assert_matches(
        "coverage_codecs_interface",
        "kt",
        &istmo_build::generate_kotlin_codecs_interface(&c),
    );
}

#[test]
fn coverage_swift_matches_golden() {
    let c = coverage();
    assert_matches("coverage_types", "swift", &generate_swift_types(&c));
    assert_matches("coverage_codecs", "swift", &generate_swift_codecs(&c));
    assert_matches("coverage_host", "swift", &generate_swift_host(&c));
    assert_matches("coverage_client", "swift", &generate_swift_client(&c));
}

#[test]
fn coverage_rust_matches_golden() {
    assert_matches("coverage_types", "rs", &generate_rust_types(&coverage()));
}

#[test]
fn coverage_byte_wide_integers_use_raw_byte_helpers() {
    let c = coverage();
    let kt = generate_kotlin_host(&c);
    assert!(kt.contains("Bincode.readU8(payload, cursor)"), "{kt}");
    assert!(kt.contains("Bincode.readI8(payload, cursor)"), "{kt}");
    let codecs = generate_kotlin_codecs(&c);
    assert!(
        codecs.contains("Bincode.writeU8(out, value.byte)"),
        "{codecs}"
    );
    assert!(
        codecs.contains("Bincode.writeI8(out, value.signedByte)"),
        "{codecs}"
    );
    let swift = generate_swift_codecs(&c);
    assert!(swift.contains("try Bincode.readU8(&c)"), "{swift}");
    assert!(
        swift.contains("Bincode.writeI8(&out, value.signedByte)"),
        "{swift}"
    );
}

#[test]
fn coverage_escapes_keywords_and_literals() {
    let c = coverage();
    let kt = generate_kotlin_host(&c);
    assert!(kt.contains("suspend fun `object`(`in`: UByte, `val`: Byte): Everything"));
    assert!(kt.contains(r#"const val PLUGIN_ID = "com.example.\"cov\$\"""#));
    let swift = generate_swift_host(&c);
    assert!(swift.contains("func `default`() async throws -> Void"));
    assert!(swift.contains(r#"PLUGIN_ID: String = "com.example.\"cov$\"""#));
    let types = generate_swift_types(&c);
    assert!(types.contains("case `in` = 0"));
    assert!(types.contains("public let `where`: Int64?"));
}

#[test]
fn coverage_clients_raise_one_exception_per_error_type() {
    let c = coverage();
    let kt = generate_kotlin_client(&c);
    assert!(kt.contains("class CoverageFirstErrorException("));
    assert!(kt.contains("class CoverageSecondErrorException("));
    assert!(kt.contains("throw CoverageSecondErrorException(e.payload, decoded)"));
    let swift = generate_swift_client(&c);
    assert!(swift.contains("public struct CoverageFirstErrorException: Error"));
    assert!(swift.contains("try? codecs.readSecondError(&errCursor)"));
}

/// The files `emit_app` writes for a host-role app whose package is not
/// `dev.istmo.runtime`: `live_activity` declares its own
/// `[plugin] android_package`, `coverage` lands in the app's package.
/// Kept whole (package line and runtime imports included) so CI compiles
/// them exactly as an app receives them.
#[test]
fn packaged_kotlin_matches_golden() {
    let root = std::env::temp_dir().join(format!("istmo-packaged-kt-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let android_root = root.join("android");
    fs::create_dir_all(android_root.join("app/src/main/java")).unwrap();
    let plugin = istmo_build::Manifest::parse(
        "[plugin]\nid = \"istmo.live_activity\"\n\
         android_package = \"dev.istmo.plugins.liveactivity\"\n",
    )
    .unwrap();
    istmo_build::emit_app_with(istmo_build::AppOpts {
        root: Some(root.clone()),
        android_root: Some(android_root.clone()),
        android_package: Some("com.example.gen".to_owned()),
        ios: Some(false),
        auto_register: Some(false),
        extra_manifests: vec![plugin],
        extra_contracts: vec![live_activity(), coverage()],
        ..istmo_build::AppOpts::default()
    });
    let java = android_root.join("app/src/main/java");
    for (dir, ty, prefix) in [
        (
            "dev/istmo/plugins/liveactivity",
            "LiveActivity",
            "packaged_live_activity",
        ),
        ("com/example/gen", "Coverage", "packaged_coverage"),
    ] {
        for (file, part) in [
            ("Types", "types"),
            ("CodecsImpl", "codecs"),
            ("Dispatcher", "host"),
        ] {
            let path = java.join(dir).join(format!("{ty}{file}.kt"));
            let source = fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
            assert_matches(&format!("{prefix}_{part}"), "kt", &source);
        }
    }
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn types_only_contract_stays_out_of_plugin_registries() {
    let types_only = Contract {
        plugin_id: "demo.shared".to_owned(),
        type_name: "Shared".to_owned(),
        methods: vec![],
        init: None,
        types: vec![TypeDef::Struct(StructDef {
            name: "Payload".to_owned(),
            fields: vec![Field {
                name: "value".to_owned(),
                ty: TypeRef::U32,
            }],
        })],
    };
    let entries: Vec<RegistryEntry> = RegistryEntry::from_contract(&types_only)
        .into_iter()
        .collect();
    assert!(entries.is_empty());
    let kotlin = generate_kotlin_plugin_registry("com.example.gen", &entries);
    assert!(!kotlin.contains("SharedBackendImpl"), "{kotlin}");
    let swift = generate_swift_plugin_registry(&entries);
    assert!(!swift.contains("SharedBackendImpl"), "{swift}");
}
