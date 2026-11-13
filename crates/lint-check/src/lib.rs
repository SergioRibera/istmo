//! Every shape the istmo macros accept, plus Rust that `istmo-build`
//! generates for `include!`, compiled under the strict lint set in this
//! crate's manifest. Code a macro emits into a user crate is linted as
//! that crate's own, so any warning here is one every such app would see.

#![deny(warnings)]

mod private;
pub mod wire;

use istmo::plugins::{ServiceContext, TaskOutcome, WorkerContext};
use istmo::{CancelToken, NativeHandleId, message, plugin};

/// Generated wire types, as `istmo-plugins` includes them.
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/generated_types.rs"));
}

/// Marker for the platform object behind [`Account::credential`].
#[non_exhaustive]
#[derive(Debug)]
pub enum Credential {}

/// Configuration a [`Gadget`] instance is created with.
#[message]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GadgetConfig {
    /// Instance name.
    pub name: String,
}

/// Domain error of [`Gadget`].
#[message]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GadgetError {
    /// The backend failed.
    Backend(String),
    /// The user said no.
    Denied,
    /// Code and detail.
    Coded(u32, String),
}

impl std::fmt::Display for GadgetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Backend(detail) => write!(f, "backend failed: {detail}"),
            Self::Denied => f.write_str("denied"),
            Self::Coded(code, detail) => write!(f, "error {code}: {detail}"),
        }
    }
}

impl std::error::Error for GadgetError {}

/// Credentials; the password never reaches `Debug` output.
#[message]
pub struct Login {
    /// User name.
    pub user: String,
    /// Password.
    #[redact]
    pub password: String,
}

/// Signed-in account carrying a native credential.
#[message]
pub struct Account {
    /// E-mail address.
    pub email: String,
    /// Platform credential.
    #[handle(Credential)]
    pub credential: NativeHandleId,
    /// Session token.
    #[redact]
    pub token: String,
}

/// Stateful plugin with every method shape.
#[plugin(name = "lint.gadget", init = GadgetConfig)]
pub trait Gadget {
    /// Echoes `text`.
    async fn ping(&self, text: String) -> String;
    /// Clears state.
    async fn reset(&self);
    /// Looks `key` up.
    async fn fetch(&self, key: String) -> Result<Option<wire::Shape>, GadgetError>;
    /// Stores `blob`.
    async fn store(&self, blob: Vec<u8>) -> Result<(), GadgetError>;
    /// Waits until cancelled.
    async fn wait(&self, millis: u64, cancel: CancelToken) -> Result<u32, GadgetError>;
    /// Signs in.
    #[istmo::owned]
    async fn sign_in(&self, login: Login) -> Result<Account, GadgetError>;
    /// Every signed-in account.
    #[istmo::owned]
    async fn accounts(&self) -> Vec<Account>;
    /// The last account, if any.
    #[istmo::owned]
    async fn last(&self) -> Option<Account>;
    /// Counts up to `count`.
    #[istmo::stream]
    fn ticks(&self, count: u32) -> u32;
    /// Shapes until one fails.
    #[istmo::stream]
    fn shapes(&self) -> Result<wire::Shape, GadgetError>;
    /// More arguments than clippy likes; the trait owns that choice.
    #[expect(clippy::too_many_arguments, reason = "mirrors a wide platform API")]
    async fn wide(&self, a: u8, b: u16, c: u32, d: u64, e: i8, f: i16, g: i32, h: i64) -> bool;
}

/// Stateless plugin without docs on its methods.
#[plugin(name = "lint.plain")]
#[expect(
    missing_docs,
    reason = "the generated client documents what the trait leaves out"
)]
pub trait Plain {
    async fn echo(&self, text: String) -> String;
    #[istmo::stream]
    fn events(&self) -> String;
}

/// Host-side [`Plain`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Loopback;

impl Plain for Loopback {
    async fn echo(&self, text: String) -> String {
        text
    }

    fn events(&self) -> flume::Receiver<String> {
        flume::unbounded().1
    }
}

/// Plugin routed to another process.
#[plugin(name = "lint.compute")]
pub trait Compute {
    /// Squares `input`.
    async fn crunch(&self, input: u32) -> u32;
}

/// Failure of [`Sync`] and [`Fallible`].
#[message]
#[derive(Debug, Clone, Copy)]
pub struct SyncError;

impl std::fmt::Display for SyncError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("sync failed")
    }
}

impl std::error::Error for SyncError {}

/// Background sync service.
#[istmo::service(name = "lint.sync")]
pub trait Sync {
    /// Runs until stopped.
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError>;
    /// Releases resources.
    async fn on_stop(&self);
}

/// [`Sync`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Syncer;

impl Sync for Syncer {
    async fn on_start(&self, ctx: ServiceContext) -> Result<(), SyncError> {
        ctx.stopped().await;
        Ok(())
    }

    async fn on_stop(&self) {}
}

/// Nightly backup worker.
#[istmo::worker(name = "lint.backup")]
pub trait Backup {
    /// Runs one backup.
    async fn run(&self, ctx: WorkerContext) -> TaskOutcome;
}

/// [`Backup`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub struct Nightly;

impl Backup for Nightly {
    async fn run(&self, _ctx: WorkerContext) -> TaskOutcome {
        TaskOutcome::Success
    }
}

/// Worker whose run can fail.
#[istmo::worker(name = "lint.fallible")]
pub trait Fallible {
    /// Runs once.
    async fn run(&self, ctx: WorkerContext) -> Result<TaskOutcome, SyncError>;
}

istmo::runtime!(
    plugins: [GadgetClient, private::MirrorClient],
    remote: [Compute],
    hosts: [Plain => Loopback, private::Mirror => private::MirrorImpl],
    services: [Sync => Syncer, private::Quiet => private::QuietImpl],
    workers: [Backup => Nightly, private::Chore => private::ChoreImpl],
);

/// Entry point the mobile shells call.
#[istmo::mobile_app]
const fn mobile_main() {}

/// The app icon `istmo-build` staged, if any.
#[must_use]
pub const fn icon() -> Option<&'static [u8]> {
    istmo::app_icon!()
}
