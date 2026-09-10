//! Byte-for-byte snapshot of the `SignIn` `Contract` extracted from
//! `src/lib.rs`.
//!
//! Guards against silent drift between the trait declaration and the
//! Kotlin / Swift host-side glue that downstream consumers generate off
//! the extracted contract. Any change to the trait — new method, renamed
//! argument, altered error variant, added `#[handle]` — surfaces here
//! and requires an intentional fixture update.
//!
//! Requires the `codegen` feature (that is the only build path where
//! `istmo-build` is linked as a runtime dep of this crate).

#![cfg(feature = "codegen")]

use istmo_build::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}

fn opt(inner: TypeRef) -> TypeRef {
    TypeRef::Option(Box::new(inner))
}

fn vec_of(inner: TypeRef) -> TypeRef {
    TypeRef::Vec(Box::new(inner))
}

fn field(name: &str, ty: TypeRef) -> Field {
    Field { name: name.to_owned(), ty }
}

fn unit_variant(name: &str) -> EnumVariant {
    EnumVariant { name: name.to_owned(), payload: Vec::new() }
}

fn payload_variant(name: &str, payload: TypeRef) -> EnumVariant {
    EnumVariant { name: name.to_owned(), payload: vec![payload] }
}

fn expected_contract() -> Contract {
    Contract {
        plugin_id: "istmo.google_sign_in".to_owned(),
        type_name: "SignIn".to_owned(),
        methods: vec![
            Method {
                name: "sign_in".to_owned(),
                kind: MethodKind::Unary,
                args: vec![Arg { name: "mode".to_owned(), ty: named("SignInMode") }],
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
                args: vec![Arg { name: "credential".to_owned(), ty: named("NativeHandleId") }],
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
            TypeDef::Enum(EnumDef {
                name: "SignInMode".to_owned(),
                variants: vec![unit_variant("Interactive"), unit_variant("SilentOnly")],
            }),
            TypeDef::Struct(StructDef {
                name: "SignInConfig".to_owned(),
                fields: vec![
                    field("serverClientId", TypeRef::String),
                    field("scopes", vec_of(TypeRef::String)),
                    field("hostedDomain", opt(TypeRef::String)),
                    field("nonce", opt(TypeRef::String)),
                    field("autoSelect", TypeRef::Bool),
                ],
            }),
            TypeDef::Struct(StructDef {
                name: "SignInAccount".to_owned(),
                fields: vec![
                    field("id", TypeRef::String),
                    field("email", opt(TypeRef::String)),
                    field("displayName", opt(TypeRef::String)),
                    field("photoUrl", opt(TypeRef::String)),
                    field("idToken", TypeRef::String),
                    field("grantedScopes", vec_of(TypeRef::String)),
                    field("credential", named("NativeHandleId")),
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "SignInError".to_owned(),
                variants: vec![
                    unit_variant("UserCancelled"),
                    unit_variant("NoCredentialAvailable"),
                    unit_variant("Reauthenticate"),
                    payload_variant("InvalidConfiguration", TypeRef::String),
                    payload_variant("Network", TypeRef::String),
                    payload_variant("Backend", TypeRef::String),
                ],
            }),
        ],
    }
}

#[test]
fn extracted_contract_matches_fixture() {
    let got = istmo_google_sign_in::codegen::contract();
    let expected = expected_contract();
    if got != expected {
        panic!(
            "extracted contract drifted from the fixture in `tests/contract_snapshot.rs`.\n\
             Update either the trait in `src/lib.rs` or the fixture — but never silently.\n\n\
             got:\n{got:#?}\n\nexpected:\n{expected:#?}",
        );
    }
}
