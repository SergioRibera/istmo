//! `Contract` builders for every bundled plugin.
//!
//! Downstream `build.rs` scripts consume these to emit Kotlin / Swift
//! host dispatchers via [`istmo_build::generate_kotlin_host`] /
//! [`istmo_build::generate_swift_host`] without duplicating the trait
//! shape at the demo's own build layer.
//!
//! Only compiled behind the `codegen` cargo feature — off by default so
//! runtime consumers of `istmo-plugins` do not pull `istmo-build`.
//!
//! Each builder must stay in lockstep with the matching `#[istmo::plugin]`
//! trait declaration; the golden test suite in `istmo-build` covers the
//! generated output byte-for-byte and catches drift.

use istmo_build::{Arg, Contract, Method, MethodKind, TypeRef};

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}

fn vec(inner: TypeRef) -> TypeRef {
    TypeRef::Vec(Box::new(inner))
}

fn opt(inner: TypeRef) -> TypeRef {
    TypeRef::Option(Box::new(inner))
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
    }
}

#[must_use]
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
    }
}
