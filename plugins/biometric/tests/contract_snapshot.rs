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

fn unit_enum(name: &str, variants: &[&str]) -> TypeDef {
    TypeDef::Enum(EnumDef {
        name: name.to_owned(),
        variants: variants
            .iter()
            .map(|v| EnumVariant {
                name: (*v).to_owned(),
                payload: Vec::new(),
            })
            .collect(),
    })
}

fn variant(name: &str, payload: Vec<TypeRef>) -> EnumVariant {
    EnumVariant {
        name: name.to_owned(),
        payload,
    }
}

fn unary(name: &str, args: Vec<Arg>, returns: TypeRef) -> Method {
    Method {
        name: name.to_owned(),
        kind: MethodKind::Unary,
        args,
        returns,
        error: Some(named("BiometricError")),
    }
}

fn expected_contract() -> Contract {
    Contract {
        plugin_id: "istmo.biometric".to_owned(),
        type_name: "Biometric".to_owned(),
        methods: expected_methods(),
        init: None,
        types: expected_types(),
    }
}

fn expected_methods() -> Vec<Method> {
    vec![
        unary(
            "availability",
            vec![arg("policy", named("AuthPolicy"))],
            named("Availability"),
        ),
        unary(
            "authenticate",
            vec![arg("prompt", named("AuthPrompt"))],
            named("AuthMethod"),
        ),
        unary(
            "store_secret",
            vec![
                arg("alias", TypeRef::String),
                arg("secret", TypeRef::Bytes),
                arg("prompt", named("AuthPrompt")),
            ],
            TypeRef::Unit,
        ),
        unary(
            "read_secret",
            vec![
                arg("alias", TypeRef::String),
                arg("prompt", named("AuthPrompt")),
            ],
            TypeRef::Bytes,
        ),
        unary(
            "delete_secret",
            vec![arg("alias", TypeRef::String)],
            TypeRef::Unit,
        ),
        unary(
            "has_secret",
            vec![arg("alias", TypeRef::String)],
            TypeRef::Bool,
        ),
    ]
}

fn expected_types() -> Vec<TypeDef> {
    vec![
        unit_enum(
            "AuthPolicy",
            &[
                "BiometricStrong",
                "BiometricWeak",
                "BiometricOrDeviceCredential",
            ],
        ),
        unit_enum(
            "BiometricStatus",
            &[
                "Available",
                "NoneEnrolled",
                "NoHardware",
                "HardwareUnavailable",
                "LockedOut",
                "SecurityUpdateRequired",
                "Unsupported",
            ],
        ),
        unit_enum("BiometricKind", &["Fingerprint", "Face", "Iris", "Other"]),
        TypeDef::Struct(StructDef {
            name: "Availability".to_owned(),
            fields: vec![
                field("status", named("BiometricStatus")),
                field("kinds", vec_of(named("BiometricKind"))),
                field("deviceCredentialAvailable", TypeRef::Bool),
            ],
        }),
        TypeDef::Struct(StructDef {
            name: "AuthPrompt".to_owned(),
            fields: vec![
                field("title", TypeRef::String),
                field("reason", TypeRef::String),
                field("subtitle", opt(TypeRef::String)),
                field("cancelLabel", opt(TypeRef::String)),
                field("fallbackLabel", opt(TypeRef::String)),
                field("policy", named("AuthPolicy")),
                field("confirmationRequired", TypeRef::Bool),
            ],
        }),
        unit_enum(
            "AuthMethod",
            &["Biometric", "DeviceCredential", "Unspecified"],
        ),
        TypeDef::Enum(EnumDef {
            name: "BiometricError".to_owned(),
            variants: vec![
                variant("UserCancelled", vec![]),
                variant("SystemCancelled", vec![]),
                variant("UserFallback", vec![]),
                variant("NotAvailable", vec![named("BiometricStatus")]),
                variant("LockedOut", vec![]),
                variant("LockedOutPermanent", vec![]),
                variant("AuthFailed", vec![]),
                variant("InvalidPrompt", vec![TypeRef::String]),
                variant("InvalidAlias", vec![TypeRef::String]),
                variant("SecretNotFound", vec![]),
                variant("KeyInvalidated", vec![]),
                variant("UnsupportedOperation", vec![TypeRef::String]),
                variant("Backend", vec![TypeRef::String]),
            ],
        }),
    ]
}

#[test]
fn extracted_contract_matches_fixture() {
    let got = istmo_biometric::codegen::contract();
    let expected = expected_contract();
    assert!(
        got == expected,
        "extracted contract drifted from the fixture in `tests/contract_snapshot.rs`.\n\
         Update either the trait in `src/lib.rs` or the fixture — but never silently.\n\n\
         got:\n{got:#?}\n\nexpected:\n{expected:#?}",
    );
}
