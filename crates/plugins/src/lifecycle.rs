//! App lifecycle stream.
//!
//! Publishes coarse-grained lifecycle states (foreground, background,
//! terminated, …) through a [`LatestValueSlot`] so late subscribers see
//! the current state immediately on attach.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

/// Early-event channel name lifecycle updates are published on.
pub const LIFECYCLE_CHANNEL: &str = "istmo.lifecycle";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LifecycleState {
    Created,
    Started,
    Resumed,
    Paused,
    Stopped,
    Destroyed,
    LowMemory,
    ConfigurationChanged,
}

#[derive(Debug, Clone)]
pub struct AppLifecycle {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for AppLifecycle {
    const PLUGIN_ID: &'static str = "istmo.lifecycle";
}

impl AppLifecycle {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(LIFECYCLE_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<LifecycleState>, IstmoError> {
        match self.slot.peek() {
            None => Ok(None),
            Some(bytes) => {
                let (state, _) = codec::decode::<LifecycleState>(&bytes)?;
                Ok(Some(state))
            }
        }
    }

    #[must_use]
    pub fn stream(&self) -> LifecycleStream {
        LifecycleStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct LifecycleStream {
    rx: Receiver<Vec<u8>>,
}

impl LifecycleStream {
    pub fn recv(&self) -> Result<LifecycleState, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<LifecycleState, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<LifecycleState, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<LifecycleState, IstmoError> {
    let (state, _) = codec::decode::<LifecycleState>(bytes)?;
    Ok(state)
}
