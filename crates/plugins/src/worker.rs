//! `WorkManager`-shaped background work.
//!
//! A **worker** is a Rust trait implementation that the platform invokes as
//! a single-shot deferrable task. On Android this maps onto `WorkManager`'s
//! `CoroutineWorker`; on iOS onto `BGProcessingTask`. The Rust side sees
//! only a single `async fn run(&self, ctx: WorkerContext) -> Result<TaskOutcome, E>`
//! method — the platform decides when to call it based on the constraints
//! attached at schedule time.
//!
//! Two pieces live here:
//!
//! * [`WorkerContext`] — passed to the impl on `run`. Carries the task id
//!   and the input payload; may grow (progress reporting, cancellation
//!   token) as real workloads land.
//! * [`Constraints`] / [`TaskOutcome`] / [`NetworkKind`] — wire message
//!   types the scheduling API and the platform Worker subclass agree on.
//!
//! The `#[istmo::worker]` macro (in `istmo-macros`) generates the adapter
//! that hosts inbound `"run"` calls.

use std::sync::Arc;

use istmo_core::{CancelToken, Runtime};
use istmo_macros::message;

/// Network requirement expressed at schedule time.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkKind {
    /// No network requirement.
    #[default]
    NotRequired,
    /// Any connected network.
    Connected,
    /// Unmetered (Wi-Fi / Ethernet).
    Unmetered,
    /// Metered network (LTE, cellular).
    Metered,
}

/// Full constraint bundle passed to the scheduler.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Constraints {
    pub required_network: NetworkKind,
    pub requires_charging: bool,
    pub requires_device_idle: bool,
    pub requires_battery_not_low: bool,
    pub requires_storage_not_low: bool,
}

/// Terminal outcome the platform expects back. Maps 1:1 onto `WorkManager`'s
/// `Result.success()` / `Result.retry()` / `Result.failure()`.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    Success,
    Retry,
    Failure,
}

/// Context handed to a worker's `run` method.
///
/// The context is intentionally minimal: workers are one-shot, so there is
/// no long-lived state to expose. Room to grow: `.set_progress(...)`,
/// cancellation tokens, and typed input decoding helpers.
#[derive(Debug)]
pub struct WorkerContext {
    runtime: Arc<Runtime>,
    task_id: String,
    unique_name: String,
    input: Vec<u8>,
    cancel: CancelToken,
}

impl WorkerContext {
    /// Construct a context. Intended for codegen and tests.
    #[must_use]
    pub const fn new(
        runtime: Arc<Runtime>,
        task_id: String,
        unique_name: String,
        input: Vec<u8>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            runtime,
            task_id,
            unique_name,
            input,
            cancel,
        }
    }

    /// Cooperative cancellation token for this run. Trips when the platform
    /// side (`WorkManager` `stop()` on Android, `BGTask.expirationHandler` on
    /// iOS) issues a `Frame::Cancel` for the `run` call. Impls should poll
    /// [`CancelToken::is_cancelled`] between logical steps and return
    /// [`TaskOutcome::Retry`] (or similar) to yield the slot cleanly.
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Non-blocking probe on the cancellation token — equivalent to
    /// `self.cancel_token().is_cancelled()`.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    /// Wire task id (matches the annotated trait's `name`).
    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    /// Unique name assigned at schedule time (`WorkManager` `enqueueUniqueWork`).
    #[must_use]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    /// Raw input payload (bincode-encoded per the trait's contract).
    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    /// Consumes the context and returns the input payload by value.
    #[must_use]
    pub fn into_input(self) -> Vec<u8> {
        self.input
    }

    /// Access the underlying runtime — same escape hatch
    /// [`crate::ServiceContext::runtime`] provides.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }
}
