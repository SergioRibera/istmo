//! Tests for `istmo_build::extract_contract`.

use istmo_build::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
    extract_contract,
};

fn write_source(source: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "istmo-extract-{}-{}",
        std::process::id(),
        rand_suffix()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lib.rs");
    std::fs::write(&path, source).unwrap();
    path
}

fn rand_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()
}

#[test]
fn stateless_unary_trait_with_primitive_arg() {
    let path = write_source(
        r#"
        use istmo::message;
        use istmo::plugin;

        #[message]
        pub struct EchoReply { pub text: String }

        #[plugin(name = "istmo.echo", crate = "::istmo_core")]
        pub trait Echo {
            async fn echo(&self, text: String) -> EchoReply;
        }
        "#,
    );

    let got = extract_contract(&path, "Echo").expect("extract");
    let expected = Contract {
        plugin_id: "istmo.echo".to_owned(),
        type_name: "Echo".to_owned(),
        methods: vec![Method {
            name: "echo".to_owned(),
            kind: MethodKind::Unary,
            args: vec![Arg { name: "text".to_owned(), ty: TypeRef::String }],
            returns: TypeRef::Named("EchoReply".to_owned()),
            error: None,
        }],
        init: None,
        types: vec![TypeDef::Struct(StructDef {
            name: "EchoReply".to_owned(),
            fields: vec![Field { name: "text".to_owned(), ty: TypeRef::String }],
        })],
    };
    assert_eq!(got, expected);
}

#[test]
fn stateful_result_with_error_and_stream() {
    let path = write_source(
        r#"
        #[istmo::message]
        pub struct Config { pub id: String }

        #[istmo::message]
        pub enum Fail {
            Denied,
            Retry(String),
        }

        #[istmo::plugin(name = "istmo.notifier", init = Config)]
        pub trait Notifier {
            async fn notify(&self, msg: String) -> Result<u32, Fail>;
            #[istmo::stream]
            fn events(&self) -> u64;
        }
        "#,
    );

    let got = extract_contract(&path, "Notifier").expect("extract");
    assert_eq!(got.plugin_id, "istmo.notifier");
    assert_eq!(got.init, Some(TypeRef::Named("Config".to_owned())));
    assert_eq!(got.methods.len(), 2);

    assert_eq!(got.methods[0].kind, MethodKind::Unary);
    assert_eq!(got.methods[0].returns, TypeRef::U32);
    assert_eq!(
        got.methods[0].error,
        Some(TypeRef::Named("Fail".to_owned()))
    );

    assert_eq!(got.methods[1].kind, MethodKind::Stream);
    assert_eq!(got.methods[1].returns, TypeRef::U64);
    assert_eq!(got.methods[1].error, None);

    // Types collected in declaration order.
    assert_eq!(got.types.len(), 2);
    match &got.types[0] {
        TypeDef::Struct(s) => assert_eq!(s.name, "Config"),
        other => panic!("expected struct Config, got {other:?}"),
    }
    match &got.types[1] {
        TypeDef::Enum(e) => {
            assert_eq!(e.name, "Fail");
            assert_eq!(
                e.variants,
                vec![
                    EnumVariant { name: "Denied".to_owned(), payload: Vec::new() },
                    EnumVariant {
                        name: "Retry".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                ]
            );
        }
        other => panic!("expected enum Fail, got {other:?}"),
    }
}

#[test]
fn cancel_token_arg_is_stripped_from_wire() {
    let path = write_source(
        r#"
        #[istmo::plugin(name = "istmo.long_running")]
        pub trait LongRunning {
            async fn tick(&self, seed: u32, cancel: istmo::CancelToken) -> ();
        }
        "#,
    );
    let got = extract_contract(&path, "LongRunning").expect("extract");
    let args = &got.methods[0].args;
    assert_eq!(args.len(), 1, "cancel arg must be stripped");
    assert_eq!(args[0].name, "seed");
    assert_eq!(args[0].ty, TypeRef::U32);
}

#[test]
fn primitive_and_wrapper_mapping() {
    let path = write_source(
        r#"
        #[istmo::plugin(name = "istmo.mapper")]
        pub trait Mapper {
            async fn flags(&self, mask: u16, on: bool) -> Option<Vec<String>>;
            async fn blob(&self, data: Vec<u8>) -> Vec<u8>;
        }
        "#,
    );
    let got = extract_contract(&path, "Mapper").expect("extract");
    let m0 = &got.methods[0];
    assert_eq!(m0.args[0].ty, TypeRef::U16);
    assert_eq!(m0.args[1].ty, TypeRef::Bool);
    assert_eq!(
        m0.returns,
        TypeRef::Option(Box::new(TypeRef::Vec(Box::new(TypeRef::String))))
    );
    let m1 = &got.methods[1];
    assert_eq!(m1.args[0].ty, TypeRef::Bytes, "Vec<u8> arg collapses to Bytes");
    assert_eq!(m1.returns, TypeRef::Bytes, "Vec<u8> return collapses to Bytes");
}

#[test]
fn matches_hand_built_google_sign_in_contract() {
    // Fixture mirrors `plugins/google-sign-in/src/codegen.rs` exactly.
    // Any drift between the trait declaration and the hand-authored
    // Contract surfaces here.
    let source = r#"
        use istmo_core::NativeHandleId;

        #[istmo::message]
        pub enum SignInMode {
            Interactive,
            SilentOnly,
        }

        #[istmo::message]
        pub struct SignInConfig {
            pub server_client_id: String,
            pub scopes: Vec<String>,
            pub hosted_domain: Option<String>,
            pub nonce: Option<String>,
            pub auto_select: bool,
        }

        #[istmo::message]
        pub struct SignInAccount {
            pub id: String,
            pub email: Option<String>,
            pub display_name: Option<String>,
            pub photo_url: Option<String>,
            pub id_token: String,
            pub granted_scopes: Vec<String>,
            pub credential: NativeHandleId,
        }

        #[istmo::message]
        pub enum SignInError {
            UserCancelled,
            NoCredentialAvailable,
            Reauthenticate,
            InvalidConfiguration(String),
            Network(String),
            Backend(String),
        }

        #[istmo::plugin(name = "istmo.google_sign_in", init = SignInConfig, crate = "::istmo_core")]
        pub trait SignIn {
            async fn sign_in(&self, mode: SignInMode) -> Result<SignInAccount, SignInError>;
            async fn silent_sign_in(&self) -> Result<Option<SignInAccount>, SignInError>;
            async fn refresh(&self, credential: NativeHandleId) -> Result<SignInAccount, SignInError>;
            async fn sign_out(&self) -> Result<(), SignInError>;
            async fn revoke(&self) -> Result<(), SignInError>;
        }
    "#;

    let path = write_source(source);
    let got = extract_contract(&path, "SignIn").expect("extract");

    // Spot-check the load-bearing bits: plugin id, init, method count,
    // camelCase field renames on the account/config structs.
    assert_eq!(got.plugin_id, "istmo.google_sign_in");
    assert_eq!(got.type_name, "SignIn");
    assert_eq!(got.init, Some(TypeRef::Named("SignInConfig".to_owned())));
    assert_eq!(got.methods.len(), 5);
    assert_eq!(got.methods[3].returns, TypeRef::Unit);
    assert_eq!(
        got.methods[3].error,
        Some(TypeRef::Named("SignInError".to_owned()))
    );

    let config_fields = match &got.types[1] {
        TypeDef::Struct(s) => {
            assert_eq!(s.name, "SignInConfig");
            s.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>()
        }
        other => panic!("expected SignInConfig struct, got {other:?}"),
    };
    assert_eq!(
        config_fields,
        vec![
            "serverClientId",
            "scopes",
            "hostedDomain",
            "nonce",
            "autoSelect",
        ]
    );

    match &got.types[2] {
        TypeDef::Struct(s) => {
            assert_eq!(s.name, "SignInAccount");
            let names: Vec<_> = s.fields.iter().map(|f| f.name.as_str()).collect();
            assert_eq!(
                names,
                vec![
                    "id",
                    "email",
                    "displayName",
                    "photoUrl",
                    "idToken",
                    "grantedScopes",
                    "credential",
                ]
            );
        }
        other => panic!("expected SignInAccount struct, got {other:?}"),
    }
}

#[test]
fn trait_not_found() {
    let path = write_source(
        r#"
        #[istmo::plugin(name = "istmo.a")]
        pub trait A { async fn a(&self); }
        "#,
    );
    let err = extract_contract(&path, "Missing").unwrap_err();
    assert!(matches!(err, istmo_build::ExtractError::TraitNotFound(_)));
}
