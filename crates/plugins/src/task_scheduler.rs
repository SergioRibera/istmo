//! Rust-side scheduling API for `#[istmo::worker]` jobs.
//!
//! Callers previously had to encode a `Frame::Call` payload by hand to
//! ask the platform to enqueue a worker. [`TaskScheduler`] wraps that in
//! a typed plugin trait: an [`enqueue`](TaskScheduler::enqueue) request
//! carries the wire task id, a unique name, worker input bytes and the
//! full [`Constraints`] bundle already used by
//! [`WorkerContext`](crate::WorkerContext); the platform side lands the
//! request on `WorkManager` (Android) or `BGTaskScheduler` (iOS).
//!
//! # Direction and lifecycle
//!
//! `TaskScheduler` is a **client-side plugin**: Rust calls, platform
//! implements. Handles returned from [`TaskScheduler::enqueue`] are
//! opaque wire ids (`WorkManager` UUIDs on Android); pass them back into
//! [`TaskScheduler::cancel`] to abort a queued run.
//!
//! # Uniqueness policy
//!
//! [`ExistingWorkPolicy`] mirrors `androidx.work.ExistingWorkPolicy`
//! semantics. Choose [`ExistingWorkPolicy::Keep`] for idempotent
//! catch-up jobs and [`ExistingWorkPolicy::Replace`] for latest-wins
//! shapes (typical for user-visible pending actions).

use istmo_macros::{message, plugin};

use crate::worker::Constraints;

/// Wire identifier of the platform-hosted task scheduler.
pub const TASK_SCHEDULER_PLUGIN_ID: &str = "istmo.task_scheduler";

/// Opaque handle returned by [`TaskScheduler::enqueue`]. On Android this
/// wraps `WorkRequest.getId().toString()`; on iOS it wraps the
/// `BGTaskScheduler.submit`-time identifier.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskHandle {
    /// Platform-assigned identifier.
    pub id: String,
}

/// Uniqueness policy for enqueue requests. Mirrors
/// `androidx.work.ExistingWorkPolicy` — the enum whose semantics every
/// Kotlin `WorkManager.enqueueUniqueWork` call has to pick from.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExistingWorkPolicy {
    /// Replace any existing unique-name run with the new request.
    #[default]
    Replace,
    /// Keep the existing run if one is scheduled — new request is a
    /// no-op.
    Keep,
    /// Append the new run after the currently-scheduled one; failures on
    /// the prior run cancel the appended one too.
    Append,
    /// Same as [`Self::Append`] but keeps the appended run when the
    /// prior one fails.
    AppendOrReplace,
}

/// A single enqueue request. `input` is the bincode-encoded argument the
/// worker's `run` method expects — the scheduler does not decode it, it
/// just ships the bytes to the platform for later delivery.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequest {
    /// Wire task id from the annotated worker trait (`#[istmo::worker(name = "…")]`).
    pub task_id: String,
    /// Unique-work name for `WorkManager.enqueueUniqueWork` — pass the
    /// same name to dedupe against an already-scheduled run per
    /// [`ExistingWorkPolicy`].
    pub unique_name: String,
    /// bincode-encoded input the worker consumes on `run`.
    pub input: Vec<u8>,
    /// Scheduling constraints (network / charging / idle / storage /
    /// battery). Reuses [`Constraints`] from the worker adapter so the
    /// wire shape stays consistent across `enqueue` and `run`.
    pub constraints: Constraints,
    /// Optional delay before the platform is allowed to run the task.
    /// `None` means "as soon as constraints are met".
    pub initial_delay_seconds: Option<u64>,
    /// Tags attached at enqueue time — used by
    /// [`TaskScheduler::cancel_by_tag`] and observability tooling.
    pub tags: Vec<String>,
    /// Policy for handling an already-scheduled unique-name run.
    pub existing_work_policy: ExistingWorkPolicy,
}

/// Domain errors the platform surfaces from a scheduling request.
/// Transport failures stay in [`istmo_core::IstmoError`]; this enum only
/// covers cases the backend legitimately reports at the domain level.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSchedulerError {
    /// `task_id` does not match any worker registered with the runtime.
    UnknownTask(String),
    /// The requested constraint combination is impossible on this
    /// platform (e.g. `requires_charging` on iOS without an entitlement).
    InvalidConstraints(String),
    /// The unique-name request conflicted with an existing run under a
    /// policy that forbids co-existence.
    Conflict(String),
    /// Free-form platform error message. Backends fall back to this when
    /// no more specific variant applies.
    Platform(String),
}

/// Client-side plugin that ships enqueue / cancel requests to the
/// platform-native scheduler.
///
/// The platform impl (Android: `androidx.work.WorkManager`; iOS:
/// [`BGTaskScheduler.submit`](https://developer.apple.com/documentation/backgroundtasks/bgtaskscheduler))
/// registers under [`TASK_SCHEDULER_PLUGIN_ID`].
#[plugin(name = "istmo.task_scheduler", crate = "::istmo_core")]
pub trait TaskScheduler {
    /// Enqueue a task. Returns the platform-assigned [`TaskHandle`].
    /// Repeat calls with the same `unique_name` are governed by the
    /// request's [`ExistingWorkPolicy`].
    async fn enqueue(&self, request: TaskRequest) -> Result<TaskHandle, TaskSchedulerError>;

    /// Cancel a specific scheduled run by handle. Backends must treat
    /// an unknown handle as a no-op (returns `Ok(())`) — a cancel
    /// racing a completion is not an error.
    async fn cancel(&self, handle: TaskHandle) -> Result<(), TaskSchedulerError>;

    /// Cancel every scheduled run carrying `tag`. No-op when no matching
    /// runs exist.
    async fn cancel_by_tag(&self, tag: String) -> Result<(), TaskSchedulerError>;

    /// Cancel every scheduled run whose unique-name matches `name`.
    /// Companion to [`Self::enqueue`]'s `unique_name` field.
    async fn cancel_by_unique_name(&self, name: String) -> Result<(), TaskSchedulerError>;
}
