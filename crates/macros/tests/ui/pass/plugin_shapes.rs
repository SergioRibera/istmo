use istmo::{CancelToken, NativeHandleId};

#[istmo::message]
#[derive(Debug)]
pub struct Failure {
    pub reason: String,
}

impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for Failure {}

pub enum Credential {}

#[istmo::message]
pub struct Account<T> {
    pub extra: T,
    #[handle(Credential)]
    pub credential: NativeHandleId,
}

/// An alias under another name is opaque and travels whole.
pub type Reply = Result<u32, Failure>;

#[istmo::plugin(name = "ui.shapes")]
pub trait Shapes {
    /// Docs are carried over to the client.
    async fn plain(&self, a: u32, b: String) -> u32;
    async fn fallible(&self) -> Result<String, Failure>;
    async fn qualified(&self) -> ::core::result::Result<u8, Failure>;
    async fn unit_error(&self) -> Result<u8, ()>;
    async fn opaque(&self) -> Reply;
    async fn nothing(&self);
    async fn cancellable(&self, delay: u32, cancel: CancelToken) -> u32;
    #[istmo::owned]
    async fn sign_in(&self) -> Result<Account<u8>, Failure>;
    #[istmo::owned]
    async fn accounts(&self) -> Vec<Account<u8>>;
    #[istmo::stream]
    fn ticks(&self, count: u32) -> u32;
}

#[istmo::message]
pub struct Config {
    pub name: String,
}

#[istmo::plugin(name = "ui.stateful", init = Config)]
pub trait Stateful {
    async fn name(&self) -> String;
}

fn main() {
    let rt = istmo::Runtime::mock().runtime;
    let client = ShapesClient::from_runtime(&rt).expect("enforcement is off in mock runtimes");
    let _: Result<istmo::TypedStream<u32>, istmo::IstmoError> = client.ticks(3);
    let _ = client.opaque();
    let _ = client.sign_in_owned();
    let _ = client.accounts_owned();
    let _ = client.cancellable(1);
}
