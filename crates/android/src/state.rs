use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use flume::Sender as FlumeSender;

use crate::error::AndroidRuntimeError;

pub(crate) struct RuntimeState {
    pub(crate) pump_shutdown: Mutex<Option<FlumeSender<()>>>,
    pub(crate) pump_join: Mutex<Option<JoinHandle<()>>>,
    shut_down: AtomicBool,
}

impl RuntimeState {
    pub(crate) const fn new(pump_shutdown: FlumeSender<()>, pump_join: JoinHandle<()>) -> Self {
        Self {
            pump_shutdown: Mutex::new(Some(pump_shutdown)),
            pump_join: Mutex::new(Some(pump_join)),
            shut_down: AtomicBool::new(false),
        }
    }

    /// Mark the runtime as shut down. Returns `false` when it already was,
    /// so a second `nativeShutdown` is a no-op.
    pub(crate) fn begin_shutdown(&self) -> bool {
        !self.shut_down.swap(true, Ordering::AcqRel)
    }

    pub(crate) fn is_shut_down(&self) -> bool {
        self.shut_down.load(Ordering::Acquire)
    }
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeState")
            .field("shut_down", &self.is_shut_down())
            .finish_non_exhaustive()
    }
}

static STATE: OnceLock<RuntimeState> = OnceLock::new();

pub(crate) fn install(state: RuntimeState) -> Result<(), AndroidRuntimeError> {
    STATE
        .set(state)
        .map_err(|_| AndroidRuntimeError::AlreadyStarted)
}

pub(crate) fn get() -> Result<&'static RuntimeState, AndroidRuntimeError> {
    STATE.get().ok_or(AndroidRuntimeError::NotStarted)
}
