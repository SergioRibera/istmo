//! Process-global handles for the transport layer.

use std::fmt;
use std::sync::{Mutex, OnceLock};
use std::thread::JoinHandle;

use flume::Sender as FlumeSender;

use crate::error::AndroidRuntimeError;

/// Handles held by the transport for the lifetime of the process runtime.
///
/// The pump thread owns its own `Arc<JavaVM>` and `GlobalRef`; nothing else
/// needs them here. This state only exists so `nativeShutdown` can stop the
/// pump thread cleanly.
pub(crate) struct RuntimeState {
    /// Signals the pump to exit. Dropping the sender closes the channel and
    /// `recv` returns `Err` — either mechanism ends the pump loop.
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
