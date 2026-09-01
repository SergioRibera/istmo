//! Application lifecycle plugin.
//!
//! The native side publishes each transition to the runtime's
//! [`LatestValueSlot`] under [`LIFECYCLE_CHANNEL`] via
//! [`Runtime::publish_early_latest`]. Late subscribers immediately observe
//! the current state; every subsequent transition is broadcast to all live
//! subscribers.
//!
//! Because the state is retained in the runtime, plugins can be constructed
//! long after the process reaches `Resumed` and still read a coherent
//! initial value — no polling and no per-plugin "did I miss the first event"
//! logic.
//!
//! [`LatestValueSlot`]: istmo_core::early_events::LatestValueSlot
//! [`Runtime::publish_early_latest`]: istmo_core::Runtime::publish_early_latest

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Runtime, codec};
use istmo_macros::message;

/// Early-event channel key the runtime publishes lifecycle transitions to.
pub const LIFECYCLE_CHANNEL: &str = "istmo.lifecycle";

/// Discrete lifecycle transitions surfaced to Rust.
///
/// The set is deliberately platform-agnostic. Android's `Activity` state
/// machine and iOS's `UIApplication` state machine both collapse to this
/// enum on the native side; the mapping lives in the platform backend.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    /// The process is initialised but the UI is not yet visible.
    Created,
    /// UI is visible but not interactive (Android `onStart`, iOS
    /// `willEnterForeground`).
    Started,
    /// UI is visible and interactive (Android `onResume`, iOS
    /// `didBecomeActive`).
    Resumed,
    /// UI is visible but has lost focus (Android `onPause`, iOS
    /// `willResignActive`).
    Paused,
    /// UI is no longer visible (Android `onStop`, iOS
    /// `didEnterBackground`).
    Stopped,
    /// The process is being torn down (Android `onDestroy`, iOS
    /// `willTerminate`).
    Destroyed,
    /// The system signalled low memory.
    LowMemory,
    /// A configuration change (locale, theme, dynamic type) has been applied.
    ConfigurationChanged,
}

/// Client for the [`LIFECYCLE_CHANNEL`] early-event slot.
#[derive(Debug, Clone)]
pub struct AppLifecycle {
    slot: Arc<LatestValueSlot>,
}

impl AppLifecycle {
    /// Attaches to the process-global runtime.
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Ok(Self::from_runtime(&rt))
    }

    /// Attaches to a specific runtime instance. Used by tests and by
    /// multi-runtime edge cases.
    #[must_use]
    pub fn from_runtime(rt: &Arc<Runtime>) -> Self {
        Self {
            slot: rt.early_events().latest_slot(LIFECYCLE_CHANNEL),
        }
    }

    /// Returns the currently retained state, if any transition has been
    /// published yet.
    pub fn current(&self) -> Result<Option<LifecycleState>, IstmoError> {
        match self.slot.peek() {
            None => Ok(None),
            Some(bytes) => {
                let (state, _) = codec::decode::<LifecycleState>(&bytes)?;
                Ok(Some(state))
            }
        }
    }

    /// Subscribes to lifecycle transitions. The returned stream first yields
    /// the currently retained state (if any), then every subsequent update.
    #[must_use]
    pub fn stream(&self) -> LifecycleStream {
        LifecycleStream {
            rx: self.slot.subscribe(),
        }
    }
}

/// Subscription handle returned by [`AppLifecycle::stream`].
#[derive(Debug)]
pub struct LifecycleStream {
    rx: Receiver<Vec<u8>>,
}

impl LifecycleStream {
    /// Blocks until the next transition arrives.
    pub fn recv(&self) -> Result<LifecycleState, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Awaits the next transition.
    pub async fn recv_async(&self) -> Result<LifecycleState, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Non-blocking read. `None` when the queue is empty.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<LifecycleState, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<LifecycleState, IstmoError> {
    let (state, _) = codec::decode::<LifecycleState>(bytes)?;
    Ok(state)
}
