//! Deep-link plugin.
//!
//! Deep links routinely arrive before the app is ready to consume them
//! (a cold-start intent, a universal link opening the app for the first
//! time). The runtime buffers each incoming link in the
//! [`PreMainQueue`] keyed by [`DEEPLINKS_CHANNEL`]; the first subscriber
//! drains the buffer and then receives live events.
//!
//! Native side publishes each link via [`Runtime::publish_early_queue`].
//!
//! [`PreMainQueue`]: istmo_core::early_events::PreMainQueue
//! [`Runtime::publish_early_queue`]: istmo_core::Runtime::publish_early_queue

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Runtime, codec};
use istmo_macros::message;

/// Early-event channel key the runtime publishes deep links to.
pub const DEEPLINKS_CHANNEL: &str = "istmo.deeplinks";

/// Default pre-main buffer capacity. Small on purpose — a cold-start app
/// realistically sees one or two queued links; excess is a bug elsewhere.
pub const DEFAULT_DEEPLINKS_CAPACITY: usize = 16;

/// A single deep link surfaced from the platform.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    /// The full URI as delivered by the OS.
    pub uri: String,
    /// Free-form origin tag (`"intent"`, `"universal-link"`, `"notification"`,
    /// ...). `None` when the platform does not attribute a source.
    pub source: Option<String>,
    /// Absolute wall-clock time (ms since the Unix epoch) at which the
    /// platform observed the link. `None` when unavailable.
    pub received_at_ms: Option<u64>,
}

/// Client for the [`DEEPLINKS_CHANNEL`] pre-main queue.
#[derive(Debug, Clone)]
pub struct DeepLinks {
    queue: Arc<PreMainQueue>,
}

impl DeepLinks {
    /// The wire plugin id this client attaches to.
    pub const PLUGIN_ID: &'static str = "istmo.deeplinks";

    /// Attaches to the process-global runtime with the default queue capacity.
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    /// Attaches to a specific runtime with the default queue capacity.
    ///
    /// # Errors
    /// Returns [`IstmoError::PluginNotDeclared`] when the runtime enforces
    /// declarations and this plugin id is missing from its `plugins:` list.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        Self::from_runtime_with_capacity(rt, DEFAULT_DEEPLINKS_CAPACITY)
    }

    /// Attaches to a specific runtime with an explicit queue capacity. The
    /// capacity is only honoured on the first attach; subsequent attaches
    /// share the queue that was created for the channel.
    ///
    /// # Errors
    /// Same as [`Self::from_runtime`].
    pub fn from_runtime_with_capacity(
        rt: &Arc<Runtime>,
        capacity: usize,
    ) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            queue: rt.early_events().queue(DEEPLINKS_CHANNEL, capacity),
        })
    }

    /// Subscribes to deep links. On first subscribe the pre-main buffer is
    /// drained into the returned stream, then live links are broadcast.
    #[must_use]
    pub fn stream(&self) -> DeepLinkStream {
        DeepLinkStream {
            rx: self.queue.subscribe(),
        }
    }
}

/// Subscription handle returned by [`DeepLinks::stream`].
#[derive(Debug)]
pub struct DeepLinkStream {
    rx: Receiver<Vec<u8>>,
}

impl DeepLinkStream {
    /// Blocks until the next link arrives.
    pub fn recv(&self) -> Result<DeepLink, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Awaits the next link.
    pub async fn recv_async(&self) -> Result<DeepLink, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Non-blocking read. `None` when the queue is empty.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<DeepLink, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<DeepLink, IstmoError> {
    let (link, _) = codec::decode::<DeepLink>(bytes)?;
    Ok(link)
}
