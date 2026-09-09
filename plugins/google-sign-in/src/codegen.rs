//! `Contract` builder for the `istmo.google_sign_in` wire surface.
//!
//! Consumed twice:
//! * By this crate's own `build.rs` to regenerate the `#[message]` shapes
//!   included from `lib.rs`.
//! * By downstream applications that need to codegen the Kotlin / Swift
//!   host dispatcher — enable the `codegen` feature and call
//!   [`contract`] from your `build.rs`.
//!
//! Keep this in lockstep with the `#[plugin]` trait declaration in `lib.rs`
//! and the `#[message]` type declarations. Any drift is caught by the
//! golden test suite in `istmo-build` (which happens to use its own inline
//! copy of the same shape as a byte-for-byte fixture).

use istmo_build::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};

#[must_use]
#[allow(unreachable_pub)] // Callers depend on the `codegen` feature; the
// `build.rs` `#[path]` include compiles this file into a separate crate
// where `pub` is meaningful, but the lib crate re-links it via
// `pub mod codegen`, so both callers see the same signature.
pub fn contract() -> Contract {
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
            enum_of(
                "SignInMode",
                vec![unit_variant("Interactive"), unit_variant("SilentOnly")],
            ),
            struct_of(
                "SignInConfig",
                vec![
                    field("serverClientId", TypeRef::String),
                    field("scopes", vec_of(TypeRef::String)),
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
                    field("grantedScopes", vec_of(TypeRef::String)),
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

fn named(name: &str) -> TypeRef {
    TypeRef::Named(name.to_owned())
}
fn vec_of(inner: TypeRef) -> TypeRef {
    TypeRef::Vec(Box::new(inner))
}
fn opt(inner: TypeRef) -> TypeRef {
    TypeRef::Option(Box::new(inner))
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
fn enum_of(name: &str, variants: Vec<EnumVariant>) -> TypeDef {
    TypeDef::Enum(EnumDef { name: name.to_owned(), variants })
}
fn struct_of(name: &str, fields: Vec<Field>) -> TypeDef {
    TypeDef::Struct(StructDef { name: name.to_owned(), fields })
}
