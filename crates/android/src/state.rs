use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use flume::Sender as FlumeSender;

use crate::error::AndroidRuntimeError;

pub(crate) struct RuntimeState {

    pub(crate) pump_shutdown: Mutex<Option<FlumeSender<()>>>,
    pub(crate) pump_join: Mutex<Option<JoinHandle<()>>>,
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeState").finish_non_exhaustive()
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

