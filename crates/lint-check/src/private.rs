//! Crate-private plugin, service and worker: the generated siblings take
//! the trait's visibility.

use istmo::CancelToken;
use istmo::plugins::{ServiceContext, TaskOutcome, WorkerContext};

/// Crate-private plugin.
#[istmo::plugin(name = "lint.mirror")]
pub(crate) trait Mirror {
    /// Echoes `text`.
    async fn echo(&self, text: String) -> String;
    /// Waits until cancelled.
    async fn wait(&self, cancel: CancelToken) -> u32;
    /// Counts.
    #[istmo::stream]
    fn ticks(&self) -> u32;
}

/// [`Mirror`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct MirrorImpl;

impl Mirror for MirrorImpl {
    async fn echo(&self, text: String) -> String {
        text
    }

    async fn wait(&self, cancel: CancelToken) -> u32 {
        cancel.cancelled().await;
        0
    }

    fn ticks(&self) -> flume::Receiver<u32> {
        flume::unbounded().1
    }
}

/// Crate-private service.
#[istmo::service(name = "lint.quiet")]
pub(crate) trait Quiet {
    /// Runs until stopped.
    async fn on_start(&self, ctx: ServiceContext);
}

/// Crate-private worker.
#[istmo::worker(name = "lint.chore")]
pub(crate) trait Chore {
    /// Runs once.
    async fn run(&self, ctx: WorkerContext) -> TaskOutcome;
}

/// [`Quiet`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct QuietImpl;

impl Quiet for QuietImpl {
    async fn on_start(&self, ctx: ServiceContext) {
        ctx.stopped().await;
    }
}

/// [`Chore`] implementation.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ChoreImpl;

impl Chore for ChoreImpl {
    async fn run(&self, _ctx: WorkerContext) -> TaskOutcome {
        TaskOutcome::Success
    }
}
