//! Inbound URL routing.
//!
//! Cold-start intents and hot URL opens are published to a
//! [`PreMainQueue`] so no link is lost when the app boots before its
//! router is ready. The default queue capacity is
//! [`DEFAULT_DEEPLINKS_CAPACITY`] — enough for a burst of taps but small
//! enough to keep memory bounded.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

/// Early-event channel name deep links are published on.
pub const DEEPLINKS_CHANNEL: &str = "istmo.deeplinks";

/// Default number of pre-subscriber links retained by the queue.
pub const DEFAULT_DEEPLINKS_CAPACITY: usize = 16;

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    pub uri: String,
    pub source: Option<String>,
    pub received_at_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct DeepLinks {
    queue: Arc<PreMainQueue>,
}

impl Plugin for DeepLinks {
    const PLUGIN_ID: &'static str = "istmo.deeplinks";
}

impl DeepLinks {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        Self::from_runtime_with_capacity(rt, DEFAULT_DEEPLINKS_CAPACITY)
    }

    pub fn from_runtime_with_capacity(
        rt: &Arc<Runtime>,
        capacity: usize,
    ) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            queue: rt.early_events().queue(DEEPLINKS_CHANNEL, capacity),
        })
    }

    #[must_use]
    pub fn stream(&self) -> DeepLinkStream {
        DeepLinkStream {
            rx: self.queue.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct DeepLinkStream {
    rx: Receiver<Vec<u8>>,
}

impl DeepLinkStream {
    pub fn recv(&self) -> Result<DeepLink, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<DeepLink, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<DeepLink, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<DeepLink, IstmoError> {
    let (link, _) = codec::decode::<DeepLink>(bytes)?;
    Ok(link)
}

