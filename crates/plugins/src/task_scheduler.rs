//! Enqueue and cancel platform-scheduled tasks.
//!
//! On Android the platform host is `WorkManager`; on Apple platforms
//! it is `BGTaskScheduler`. Both back the same
//! [`TaskScheduler`] trait.

use istmo_macros::{message, plugin};

use crate::worker::Constraints;

/// Wire identifier for the task-scheduler plugin.
pub const TASK_SCHEDULER_PLUGIN_ID: &str = "istmo.task_scheduler";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TaskHandle {
    pub id: String,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExistingWorkPolicy {
    #[default]
    Replace,
    Keep,
    Append,
    AppendOrReplace,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRequest {
    pub task_id: String,
    pub unique_name: String,
    pub input: Vec<u8>,
    pub constraints: Constraints,
    pub initial_delay_seconds: Option<u64>,
    pub tags: Vec<String>,
    pub existing_work_policy: ExistingWorkPolicy,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskSchedulerError {
    UnknownTask(String),
    InvalidConstraints(String),
    Conflict(String),
    Platform(String),
}

impl core::fmt::Display for TaskSchedulerError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownTask(id) => write!(f, "unknown task `{id}`"),
            Self::InvalidConstraints(msg) => write!(f, "invalid constraints: {msg}"),
            Self::Conflict(msg) => write!(f, "task conflict: {msg}"),
            Self::Platform(msg) => write!(f, "task scheduler platform error: {msg}"),
        }
    }
}

impl std::error::Error for TaskSchedulerError {}

#[plugin(name = "istmo.task_scheduler", crate = "::istmo_core")]
pub trait TaskScheduler {
    async fn enqueue(&self, request: TaskRequest) -> Result<TaskHandle, TaskSchedulerError>;

    async fn cancel(&self, handle: TaskHandle) -> Result<(), TaskSchedulerError>;

    async fn cancel_by_tag(&self, tag: String) -> Result<(), TaskSchedulerError>;

    async fn cancel_by_unique_name(&self, name: String) -> Result<(), TaskSchedulerError>;
}

