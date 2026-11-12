use std::sync::atomic::{AtomicU64, Ordering};

use istmo_build::{
    Arg, Contract, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
    extract_contract,
};

static COUNTER: AtomicU64 = AtomicU64::new(0);

fn write_source(source: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "istmo-extract-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed),
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("lib.rs");
    std::fs::write(&path, source).unwrap();
    path
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
            args: vec![Arg {
                name: "text".to_owned(),
                ty: TypeRef::String,
            }],
            returns: TypeRef::Named("EchoReply".to_owned()),
            error: None,
        }],
        init: None,
        types: vec![TypeDef::Struct(StructDef {
            name: "EchoReply".to_owned(),
            fields: vec![Field {
                name: "text".to_owned(),
                ty: TypeRef::String,
            }],
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

    assert_eq!(got.types.len(), 2);
    match &got.types[0] {
        TypeDef::Struct(s) => assert_eq!(s.name, "Config"),
        other @ TypeDef::Enum(_) => panic!("expected struct Config, got {other:?}"),
    }
    match &got.types[1] {
        TypeDef::Enum(e) => {
            assert_eq!(e.name, "Fail");
            assert_eq!(
                e.variants,
                vec![
                    EnumVariant {
                        name: "Denied".to_owned(),
                        payload: Vec::new()
                    },
                    EnumVariant {
                        name: "Retry".to_owned(),
                        payload: vec![TypeRef::String],
                    },
                ]
            );
        }
        other @ TypeDef::Struct(_) => panic!("expected enum Fail, got {other:?}"),
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
    assert_eq!(
        m1.args[0].ty,
        TypeRef::Bytes,
        "Vec<u8> arg collapses to Bytes"
    );
    assert_eq!(
        m1.returns,
        TypeRef::Bytes,
        "Vec<u8> return collapses to Bytes"
    );
}

#[test]
fn matches_hand_built_google_sign_in_contract() {
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
        other @ TypeDef::Enum(_) => panic!("expected SignInConfig struct, got {other:?}"),
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
        other @ TypeDef::Enum(_) => panic!("expected SignInAccount struct, got {other:?}"),
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

fn unsupported(source: &str) -> String {
    let path = write_source(source);
    match extract_contract(&path, "P") {
        Err(istmo_build::ExtractError::Unsupported(msg)) => msg,
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn rejects_types_without_a_wire_mapping() {
    for (ty, needle) in [
        ("usize", "`usize`"),
        ("u128", "`u128`"),
        ("char", "`char`"),
        ("std::collections::HashMap<String, u32>", "generic type"),
        ("Box<u32>", "generic type"),
        ("(u32, u32)", "type not supported"),
        ("Missing", "`Missing`"),
    ] {
        let msg = unsupported(&format!(
            r#"
            #[istmo::plugin(name = "p")]
            pub trait P {{ async fn go(&self, value: {ty}); }}
            "#
        ));
        assert!(msg.contains(needle), "{ty}: {msg}");
        assert!(msg.contains("method `go`"), "{ty}: {msg}");
    }
}

#[test]
fn rejects_single_argument_result_alias() {
    let msg = unsupported(
        r#"
        #[istmo::plugin(name = "p")]
        pub trait P { async fn go(&self) -> io::Result<u32>; }
        "#,
    );
    assert!(msg.contains("Result<T, E>"), "{msg}");
}

#[test]
fn rejects_nested_option_and_unknown_attribute_keys() {
    let msg = unsupported(
        r#"
        #[istmo::plugin(name = "p")]
        pub trait P { async fn go(&self) -> Option<Option<u8>>; }
        "#,
    );
    assert!(msg.contains("Option<Option"), "{msg}");

    let msg = unsupported(
        r#"
        #[istmo::plugin(name = "p", nmae = "typo")]
        pub trait P { async fn go(&self); }
        "#,
    );
    assert!(msg.contains("`nmae`"), "{msg}");
}

#[test]
fn rejects_fields_colliding_after_camel_case() {
    let msg = unsupported(
        r#"
        #[istmo::message]
        pub struct S { pub foo_bar: u8, pub fooBar: u8 }

        #[istmo::plugin(name = "p")]
        pub trait P { async fn go(&self, s: S); }
        "#,
    );
    assert!(
        msg.contains("`foo_bar`") && msg.contains("`fooBar`"),
        "{msg}"
    );
}

#[test]
fn raw_identifiers_lose_their_prefix() {
    let path = write_source(
        r#"
        #[istmo::message]
        pub struct r#Item { pub r#type: String }

        #[istmo::plugin(name = "p")]
        pub trait P { async fn r#in(&self, r#where: r#Item); }
        "#,
    );
    let got = extract_contract(&path, "P").expect("extract");
    assert_eq!(got.methods[0].name, "in");
    assert_eq!(got.methods[0].args[0].name, "where");
    assert_eq!(got.methods[0].args[0].ty, TypeRef::Named("Item".to_owned()));
    match &got.types[0] {
        TypeDef::Struct(s) => assert_eq!(s.fields[0].name, "type"),
        other @ TypeDef::Enum(_) => panic!("expected struct, got {other:?}"),
    }
}

#[test]
fn collects_message_types_from_inline_and_file_modules() {
    let path = write_source(
        r#"
        mod inline {
            #[istmo::message]
            pub struct A { pub x: u8 }
        }
        mod on_disk;

        #[istmo::plugin(name = "p")]
        pub trait P { async fn go(&self, a: inline::A, b: on_disk::B); }
        "#,
    );
    let dir = path.parent().unwrap();
    std::fs::write(
        dir.join("on_disk.rs"),
        "mod deeper;\n#[istmo::message]\npub struct B { pub inner: C }\n",
    )
    .unwrap();
    std::fs::create_dir_all(dir.join("on_disk")).unwrap();
    std::fs::write(
        dir.join("on_disk/deeper.rs"),
        "#[istmo::message]\npub enum C { One, Two(u16) }\n",
    )
    .unwrap();

    let got = extract_contract(&path, "P").expect("extract");
    let names: Vec<&str> = got.types.iter().map(TypeDef::name).collect();
    assert_eq!(names, ["A", "C", "B"], "source order, modules in place");
}

#[test]
fn path_attributes_resolve_like_rustc() {
    let path = write_source(
        r#"
        mod plain;
        #[path = "sub/loaded.rs"]
        mod loaded;

        #[istmo::plugin(name = "p")]
        pub trait P { async fn go(&self, a: plain::A, b: loaded::B); }
        "#,
    );
    let dir = path.parent().unwrap();
    std::fs::create_dir_all(dir.join("sub")).unwrap();
    // `#[path]` in a non-mod-rs file is relative to that file's directory.
    std::fs::write(
        dir.join("plain.rs"),
        "#[path = \"beside.rs\"]\nmod beside;\n#[istmo::message]\npub struct A { pub c: C }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("beside.rs"),
        "#[istmo::message]\npub struct C { pub x: u8 }\n",
    )
    .unwrap();
    // A file loaded through `#[path]` is mod-rs: `mod d;` sits beside it.
    std::fs::write(
        dir.join("sub/loaded.rs"),
        "mod d;\n#[istmo::message]\npub struct B { pub d: D }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("sub/d.rs"),
        "#[istmo::message]\npub struct D { pub y: u16 }\n",
    )
    .unwrap();

    let got = extract_contract(&path, "P").expect("extract");
    let names: Vec<&str> = got.types.iter().map(TypeDef::name).collect();
    assert_eq!(names, ["C", "A", "D", "B"]);
}

#[test]
fn parse_errors_report_line_and_column() {
    let path = write_source("pub struct Broken {\n    x: u8,,\n}\n");
    let err = extract_contract(&path, "P").unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("line 2"), "{msg}");
}
