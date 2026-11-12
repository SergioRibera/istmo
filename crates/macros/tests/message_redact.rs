//! `#[redact]` keeps secrets out of the `Debug` output of a `#[message]`
//! struct and of its `Owned*` sibling.

use istmo::{NativeHandleId, Runtime};

#[derive(Debug)]
pub enum Token {}

#[istmo::message]
#[derive(Clone, PartialEq, Eq)]
pub struct Session {
    pub user: String,
    #[redact]
    pub secret: String,
    #[istmo::redact]
    pub nonce: Option<String>,
    #[handle(Token)]
    pub token: NativeHandleId,
}

#[istmo::message]
pub struct Boxed<T> {
    pub value: T,
    #[redact]
    pub key: Vec<u8>,
}

#[istmo::message]
#[derive(Debug)]
pub struct Plain {
    pub id: u32,
    #[handle(Token)]
    pub token: NativeHandleId,
}

fn session() -> Session {
    Session {
        user: "ana".into(),
        secret: "hunter2".into(),
        nonce: Some("n-0nce".into()),
        token: NativeHandleId(7),
    }
}

#[test]
fn wire_debug_hides_redacted_fields() {
    let shown = format!("{:?}", session());
    assert_eq!(
        shown,
        r#"Session { user: "ana", secret: <redacted>, nonce: <redacted>, token: native(7) }"#,
    );
    assert!(!shown.contains("hunter2"));
    assert!(!shown.contains("n-0nce"));
}

#[test]
fn owned_debug_hides_redacted_fields() {
    let rt = Runtime::mock().runtime;
    let owned = session().into_owned(&rt);
    let shown = format!("{owned:?}");
    assert!(shown.starts_with(
        r#"OwnedSession { user: "ana", secret: <redacted>, nonce: <redacted>, token: "#
    ));
    assert!(!shown.contains("hunter2"));
    assert!(!shown.contains("n-0nce"));
}

#[test]
fn generic_struct_bounds_type_params_by_debug() {
    let boxed = Boxed {
        value: 3_u8,
        key: vec![0xde, 0xad],
    };
    assert_eq!(format!("{boxed:?}"), "Boxed { value: 3, key: <redacted> }");
}

#[test]
fn owned_debug_without_redact_lists_every_field() {
    let rt = Runtime::mock().runtime;
    let owned = Plain {
        id: 5,
        token: NativeHandleId(9),
    }
    .into_owned(&rt);
    assert!(format!("{owned:?}").starts_with("OwnedPlain { id: 5, token: "));
}
