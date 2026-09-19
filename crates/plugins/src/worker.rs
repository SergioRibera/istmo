//! Context and helpers passed to
//! [`#[istmo::worker]`](../../../istmo_macros/attr.worker.html) adapters.
//!
//! [`WorkerContext`] carries cooperative cancellation and the runtime
//! handle for outbound calls; [`Constraints`] describes when the
//! platform is willing to run the task.

use std::sync::Arc;

use istmo_core::{CancelToken, Runtime};
use istmo_macros::message;

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NetworkKind {

    #[default]
    NotRequired,

    Connected,

    Unmetered,

    Metered,
}

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

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskOutcome {
    Success,
    Retry,
    Failure,
}

#[derive(Debug)]
pub struct WorkerContext {
    runtime: Arc<Runtime>,
    task_id: String,
    unique_name: String,
    input: Vec<u8>,
    cancel: CancelToken,
}

impl WorkerContext {

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

    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_cancelled()
    }

    #[must_use]
    pub fn task_id(&self) -> &str {
        &self.task_id
    }

    #[must_use]
    pub fn unique_name(&self) -> &str {
        &self.unique_name
    }

    #[must_use]
    pub fn input(&self) -> &[u8] {
        &self.input
    }

    #[must_use]
    pub fn into_input(self) -> Vec<u8> {
        self.input
    }

    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }
}

