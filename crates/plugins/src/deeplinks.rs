//! Inbound URL routing.
//!
//! Cold-start intents and hot URL opens are published to a
//! [`PreMainQueue`] so no link is lost when the app boots before its
//! router is ready. The default queue capacity is
//! [`DEFAULT_DEEPLINKS_CAPACITY`] — enough for a burst of taps but small
//! enough to keep memory bounded.

use std::sync::Arc;

use istmo_core::early_events::PreMainQueue;
use istmo_core::{IstmoError, Plugin, Runtime};
use istmo_macros::message;

use crate::early::EarlyStream;

/// Plugin id of the deep-link queue.
pub const DEEPLINKS_PLUGIN_ID: &str = "istmo.deeplinks";

/// Early-event channel name deep links are published on.
pub const DEEPLINKS_CHANNEL: &str = "istmo.deeplinks";

/// Default number of pre-subscriber links retained by the queue.
pub const DEFAULT_DEEPLINKS_CAPACITY: usize = 16;

/// One URL the OS asked the app to open.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeepLink {
    /// The URL as received.
    pub uri: String,
    /// Where it came from (referrer app, notification, …) when known.
    pub source: Option<String>,
    /// Receive time in Unix-epoch milliseconds, when the platform
    /// stamps it.
    pub received_at_ms: Option<u64>,
}

/// Reader for the deep-link queue.
#[derive(Debug, Clone)]
pub struct DeepLinks {
    queue: Arc<PreMainQueue>,
}

impl Plugin for DeepLinks {
    const PLUGIN_ID: &'static str = DEEPLINKS_PLUGIN_ID;
}

impl DeepLinks {
    /// Client bound to the process-wide runtime.
    ///
    /// # Errors
    ///
    /// Fails when no global runtime is installed or the plugin was not
    /// declared.
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    /// Client bound to `rt` with the default queue capacity.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        Self::from_runtime_with_capacity(rt, DEFAULT_DEEPLINKS_CAPACITY)
    }

    /// Client bound to `rt`, retaining up to `capacity` links published
    /// before the first subscriber.
    ///
    /// # Errors
    ///
    /// Same as [`from_runtime`](Self::from_runtime).
    pub fn from_runtime_with_capacity(
        rt: &Arc<Runtime>,
        capacity: usize,
    ) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            queue: rt.early_events().queue(DEEPLINKS_CHANNEL, capacity),
        })
    }

    /// Subscribe: the first subscriber drains the links buffered so far.
    #[must_use]
    pub fn stream(&self) -> DeepLinkStream {
        EarlyStream::new(self.queue.subscribe())
    }
}

/// Deep links as they arrive.
pub type DeepLinkStream = EarlyStream<DeepLink>;
