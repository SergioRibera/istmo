use std::fmt;
use std::sync::{Mutex, OnceLock, PoisonError};
use std::thread;

use crate::error::IosRuntimeError;
use crate::pump::PumpHandles;

pub(crate) struct RuntimeState {
    /// `None` once [`RuntimeState::stop_pump`] ran.
    pump: Mutex<Option<PumpHandles>>,
}

impl fmt::Debug for RuntimeState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RuntimeState").finish_non_exhaustive()
    }
}

impl RuntimeState {
    pub(crate) const fn new(pump: PumpHandles) -> Self {
        Self {
            pump: Mutex::new(Some(pump)),
        }
    }

    /// Signal the pump to exit and join it. Returns `true` only when this
    /// call joined the thread. Called on the pump thread itself (shutdown
    /// from inside a callback) the join is skipped — a thread cannot join
    /// itself — and the pump exits once the running callback returns.
    pub(crate) fn stop_pump(&self) -> bool {
        // Poisoning is recovered: the guarded value is a plain `Option`
        // with no invariant a panic could break.
        let pump = self
            .pump
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        let Some(PumpHandles { shutdown, join }) = pump else {
            return false;
        };
        drop(shutdown);
        if join.thread().id() == thread::current().id() {
            return false;
        }
        if join.join().is_err() {
            tracing::error!("istmo ios pump thread panicked");
        }
        true
    }
}

static STATE: OnceLock<RuntimeState> = OnceLock::new();

pub(crate) fn install(state: RuntimeState) -> Result<(), IosRuntimeError> {
    STATE
        .set(state)
        .map_err(|_| IosRuntimeError::AlreadyStarted)
}

pub(crate) fn get() -> Result<&'static RuntimeState, IosRuntimeError> {
    STATE.get().ok_or(IosRuntimeError::NotStarted)
}
