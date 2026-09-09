//! `Contract` builders for every bundled plugin.
//!
//! Downstream `build.rs` scripts consume these to emit Kotlin / Swift
//! host dispatchers, types, and codecs via `istmo-build`'s generators
//! without duplicating trait shape or wire encoding at the demo's own
//! build layer.
//!
//! Only compiled behind the `codegen` cargo feature — off by default so
//! runtime consumers of `istmo-plugins` do not pull `istmo-build`.
//!
//! Each builder must stay in lockstep with the matching `#[istmo::plugin]`
//! trait declaration and `#[message]` type declarations. The golden test
//! suite in `istmo-build` covers the generated output byte-for-byte and
//! catches drift.

use istmo_build::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}

fn vec(inner: TypeRef) -> TypeRef {
    TypeRef::Vec(Box::new(inner))
}

fn opt(inner: TypeRef) -> TypeRef {
    TypeRef::Option(Box::new(inner))
}

fn field(name: &str, ty: TypeRef) -> Field {
    Field {
        name: name.to_owned(),
        ty,
    }
}

fn unit_variant(name: &str) -> EnumVariant {
    EnumVariant {
        name: name.to_owned(),
        payload: Vec::new(),
    }
}

fn payload_variant(name: &str, payload: TypeRef) -> EnumVariant {
    EnumVariant {
        name: name.to_owned(),
        payload: vec![payload],
    }
}

fn enum_of(name: &str, variants: Vec<EnumVariant>) -> TypeDef {
    TypeDef::Enum(EnumDef {
        name: name.to_owned(),
        variants,
    })
}

fn struct_of(name: &str, fields: Vec<Field>) -> TypeDef {
    TypeDef::Struct(StructDef {
        name: name.to_owned(),
        fields,
    })
}

#[must_use]
pub fn permissions() -> Contract {
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
                returns: named("PermissionStatus"),
                error: None,
            },
            Method {
                name: "request".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "permissions".to_owned(),
                    ty: vec(TypeRef::String),
                }],
                returns: vec(named("PermissionOutcome")),
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
        types: vec![
            enum_of(
                "PermissionStatus",
                vec![
                    unit_variant("Granted"),
                    unit_variant("Denied"),
                    unit_variant("PermanentlyDenied"),
                    unit_variant("NotDetermined"),
                    unit_variant("NotSupported"),
                ],
            ),
            struct_of(
                "PermissionOutcome",
                vec![
                    field("permission", TypeRef::String),
                    field("status", named("PermissionStatus")),
                ],
            ),
        ],
    }
}

#[must_use]
pub fn notifications() -> Contract {
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
                    ty: named("NotificationRequest"),
                }],
                returns: named("NotificationHandle"),
                error: Some(named("NotificationError")),
            },
            Method {
                name: "cancel".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "id".to_owned(),
                    ty: TypeRef::U32,
                }],
                returns: TypeRef::Unit,
                error: Some(named("NotificationError")),
            },
        ],
        init: None,
        types: vec![
            enum_of(
                "NotificationImportance",
                vec![
                    unit_variant("Min"),
                    unit_variant("Low"),
                    unit_variant("Default"),
                    unit_variant("High"),
                ],
            ),
            struct_of(
                "NotificationRequest",
                vec![
                    field("title", TypeRef::String),
                    field("body", TypeRef::String),
                    field("channelId", TypeRef::String),
                    field("importance", named("NotificationImportance")),
                    field("delaySeconds", opt(TypeRef::U32)),
                    field("tag", opt(TypeRef::String)),
                ],
            ),
            struct_of("NotificationHandle", vec![field("id", TypeRef::U32)]),
            enum_of(
                "NotificationError",
                vec![
                    unit_variant("PermissionDenied"),
                    payload_variant("InvalidChannel", TypeRef::String),
                    payload_variant("Scheduler", TypeRef::String),
                ],
            ),
        ],
    }
}

/// Contract for `istmo.service_control` — the platform-hosted plugin that
/// [`istmo_plugins::ServiceContext`] shells its foreground-notification /
/// wakelock / stop-self helpers into.
#[must_use]
#[allow(clippy::too_many_lines)]
pub fn service_control() -> Contract {
    Contract {
        plugin_id: "istmo.service_control".to_owned(),
        type_name: "ServiceControl".to_owned(),
        methods: vec![
            Method {
                name: "start_foreground".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "service_id".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "spec".to_owned(),
                        ty: named("NotificationSpec"),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(named("ServiceControlError")),
            },
            Method {
                name: "update_notification".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "service_id".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "spec".to_owned(),
                        ty: named("NotificationSpec"),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(named("ServiceControlError")),
            },
            Method {
                name: "stop_foreground".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "service_id".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "remove_notification".to_owned(),
                        ty: TypeRef::Bool,
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(named("ServiceControlError")),
            },
            Method {
                name: "acquire_wakelock".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "service_id".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "tag".to_owned(),
                        ty: TypeRef::String,
                    },
                ],
                returns: named("WakelockToken"),
                error: Some(named("ServiceControlError")),
            },
            Method {
                name: "release_wakelock".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "service_id".to_owned(),
                        ty: TypeRef::String,
                    },
                    Arg {
                        name: "token".to_owned(),
                        ty: named("WakelockToken"),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(named("ServiceControlError")),
            },
            Method {
                name: "stop_self".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "service_id".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Unit,
                error: Some(named("ServiceControlError")),
            },
        ],
        init: None,
        types: vec![
            struct_of(
                "NotificationSpec",
                vec![
                    field("channelId", TypeRef::String),
                    field("notificationId", TypeRef::I32),
                    field("title", TypeRef::String),
                    field("body", TypeRef::String),
                    field("smallIcon", opt(TypeRef::String)),
                    field("ongoing", TypeRef::Bool),
                    field("foregroundServiceType", opt(TypeRef::U32)),
                ],
            ),
            struct_of("WakelockToken", vec![field("token", TypeRef::U64)]),
            enum_of(
                "ServiceControlError",
                vec![
                    payload_variant("UnknownService", TypeRef::String),
                    payload_variant("WakelockDenied", TypeRef::String),
                    payload_variant("Platform", TypeRef::String),
                ],
            ),
        ],
    }
}

/// Contract for `istmo.task_scheduler` — the platform-hosted plugin
/// that lands `WorkManager` (Android) / `BGTaskScheduler` (iOS)
/// enqueue and cancel requests originating from Rust.
#[must_use]
pub fn task_scheduler() -> Contract {
    Contract {
        plugin_id: "istmo.task_scheduler".to_owned(),
        type_name: "TaskScheduler".to_owned(),
        methods: vec![
            Method {
                name: "enqueue".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "request".to_owned(),
                    ty: named("TaskRequest"),
                }],
                returns: named("TaskHandle"),
                error: Some(named("TaskSchedulerError")),
            },
            Method {
                name: "cancel".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "handle".to_owned(),
                    ty: named("TaskHandle"),
                }],
                returns: TypeRef::Unit,
                error: Some(named("TaskSchedulerError")),
            },
            Method {
                name: "cancel_by_tag".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "tag".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Unit,
                error: Some(named("TaskSchedulerError")),
            },
            Method {
                name: "cancel_by_unique_name".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "name".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: TypeRef::Unit,
                error: Some(named("TaskSchedulerError")),
            },
        ],
        init: None,
        types: vec![
            enum_of(
                "NetworkKind",
                vec![
                    unit_variant("NotRequired"),
                    unit_variant("Connected"),
                    unit_variant("Unmetered"),
                    unit_variant("Metered"),
                ],
            ),
            struct_of(
                "Constraints",
                vec![
                    field("requiredNetwork", named("NetworkKind")),
                    field("requiresCharging", TypeRef::Bool),
                    field("requiresDeviceIdle", TypeRef::Bool),
                    field("requiresBatteryNotLow", TypeRef::Bool),
                    field("requiresStorageNotLow", TypeRef::Bool),
                ],
            ),
            enum_of(
                "ExistingWorkPolicy",
                vec![
                    unit_variant("Replace"),
                    unit_variant("Keep"),
                    unit_variant("Append"),
                    unit_variant("AppendOrReplace"),
                ],
            ),
            struct_of("TaskHandle", vec![field("id", TypeRef::String)]),
            struct_of(
                "TaskRequest",
                vec![
                    field("taskId", TypeRef::String),
                    field("uniqueName", TypeRef::String),
                    field("input", TypeRef::Bytes),
                    field("constraints", named("Constraints")),
                    field("initialDelaySeconds", opt(TypeRef::U64)),
                    field("tags", vec(TypeRef::String)),
                    field("existingWorkPolicy", named("ExistingWorkPolicy")),
                ],
            ),
            enum_of(
                "TaskSchedulerError",
                vec![
                    payload_variant("UnknownTask", TypeRef::String),
                    payload_variant("InvalidConstraints", TypeRef::String),
                    payload_variant("Conflict", TypeRef::String),
                    payload_variant("Platform", TypeRef::String),
                ],
            ),
        ],
    }
}

#[must_use]
pub fn google_sign_in() -> Contract {
    Contract {
        plugin_id: "istmo.google_sign_in".to_owned(),
        type_name: "SignIn".to_owned(),
        methods: vec![
            Method {
                name: "sign_in".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "mode".to_owned(),
                    ty: named("SignInMode"),
                }],
                returns: named("SignInAccount"),
                error: Some(named("SignInError")),
            },
            Method {
                name: "silent_sign_in".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: opt(named("SignInAccount")),
                error: Some(named("SignInError")),
            },
            Method {
                name: "refresh".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "credential".to_owned(),
                    ty: named("NativeHandleId"),
                }],
                returns: named("SignInAccount"),
                error: Some(named("SignInError")),
            },
            Method {
                name: "sign_out".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Unit,
                error: Some(named("SignInError")),
            },
            Method {
                name: "revoke".to_owned(),
                kind: MethodKind::Unary,
                args: vec![],
                returns: TypeRef::Unit,
                error: Some(named("SignInError")),
            },
        ],
        init: Some(named("SignInConfig")),
        types: vec![
            enum_of(
                "SignInMode",
                vec![unit_variant("Interactive"), unit_variant("SilentOnly")],
            ),
            struct_of(
                "SignInConfig",
                vec![
                    field("serverClientId", TypeRef::String),
                    field("scopes", vec(TypeRef::String)),
                    field("hostedDomain", opt(TypeRef::String)),
                    field("nonce", opt(TypeRef::String)),
                    field("autoSelect", TypeRef::Bool),
                ],
            ),
            struct_of(
                "SignInAccount",
                vec![
                    field("id", TypeRef::String),
                    field("email", opt(TypeRef::String)),
                    field("displayName", opt(TypeRef::String)),
                    field("photoUrl", opt(TypeRef::String)),
                    field("idToken", TypeRef::String),
                    field("grantedScopes", vec(TypeRef::String)),
                    field("credential", named("NativeHandleId")),
                ],
            ),
            enum_of(
                "SignInError",
                vec![
                    unit_variant("UserCancelled"),
                    unit_variant("NoCredentialAvailable"),
                    unit_variant("Reauthenticate"),
                    payload_variant("InvalidConfiguration", TypeRef::String),
                    payload_variant("Network", TypeRef::String),
                    payload_variant("Backend", TypeRef::String),
                ],
            ),
        ],
    }
}

#[must_use]
#[allow(clippy::too_many_lines)]
pub fn admob() -> Contract {
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
                returns: named("NativeHandleId"),
                error: Some(named("AdError")),
            },
            Method {
                name: "show_interstitial".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad".to_owned(),
                    ty: named("NativeHandleId"),
                }],
                returns: named("InterstitialOutcome"),
                error: Some(named("AdError")),
            },
            Method {
                name: "load_rewarded".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad_unit_id".to_owned(),
                    ty: TypeRef::String,
                }],
                returns: named("NativeHandleId"),
                error: Some(named("AdError")),
            },
            Method {
                name: "show_rewarded".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "ad".to_owned(),
                    ty: named("NativeHandleId"),
                }],
                returns: named("RewardedOutcome"),
                error: Some(named("AdError")),
            },
            Method {
                name: "show_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "request".to_owned(),
                    ty: named("BannerRequest"),
                }],
                returns: named("NativeHandleId"),
                error: Some(named("AdError")),
            },
            Method {
                name: "update_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![
                    Arg {
                        name: "banner".to_owned(),
                        ty: named("NativeHandleId"),
                    },
                    Arg {
                        name: "rect".to_owned(),
                        ty: named("BannerRect"),
                    },
                ],
                returns: TypeRef::Unit,
                error: Some(named("AdError")),
            },
            Method {
                name: "hide_banner".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg {
                    name: "banner".to_owned(),
                    ty: named("NativeHandleId"),
                }],
                returns: TypeRef::Unit,
                error: Some(named("AdError")),
            },
        ],
        init: Some(named("AdMobConfig")),
        types: vec![
            struct_of(
                "AdMobConfig",
                vec![
                    field("appId", TypeRef::String),
                    field("testDeviceIds", vec(TypeRef::String)),
                    field("childDirectedTreatment", TypeRef::Bool),
                ],
            ),
            struct_of(
                "BannerRect",
                vec![
                    field("x", TypeRef::U32),
                    field("y", TypeRef::U32),
                    field("width", TypeRef::U32),
                    field("height", TypeRef::U32),
                ],
            ),
            struct_of(
                "BannerRequest",
                vec![
                    field("adUnitId", TypeRef::String),
                    field("rect", named("BannerRect")),
                ],
            ),
            enum_of(
                "InterstitialOutcome",
                vec![unit_variant("Dismissed"), unit_variant("FailedToShow")],
            ),
            struct_of(
                "RewardedOutcome",
                vec![
                    field("granted", TypeRef::Bool),
                    field("rewardType", TypeRef::String),
                    field("rewardAmount", TypeRef::U32),
                ],
            ),
            enum_of(
                "AdError",
                vec![
                    unit_variant("NotInitialized"),
                    unit_variant("NoFill"),
                    payload_variant("Network", TypeRef::String),
                    payload_variant("InvalidRequest", TypeRef::String),
                    unit_variant("UnknownAd"),
                    payload_variant("Internal", TypeRef::String),
                ],
            ),
        ],
    }
}
