use istmo::NativeHandleId;

pub enum Credential {}

#[istmo::message]
#[derive(Clone, PartialEq)]
pub struct Account {
    pub id: String,
    #[redact]
    pub id_token: String,
    #[handle(Credential)]
    pub credential: NativeHandleId,
}

fn main() {
    let account = Account {
        id: "1".into(),
        id_token: "eyJ.secret".into(),
        credential: NativeHandleId(1),
    };
    assert!(!format!("{account:?}").contains("eyJ"));
    let rt = istmo::Runtime::mock().runtime;
    assert!(!format!("{:?}", account.into_owned(&rt)).contains("eyJ"));
}
