//! Reader of the [`LaunchReason`] lane.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Runtime, codec};

use crate::{AUTOSTART_PLUGIN_ID, LAUNCH_CHANNEL, LaunchReason};

/// A `LaunchReason` is a few bytes; anything bigger is not one.
const MAX_REASON_BYTES: usize = 64;

/// Reads why this process started, as [`AutostartPublisher::install`]
/// published it.
///
/// [`Self::current`] is `None` before the publish. On macOS the system
/// answer arrives once `NSApplication` finished launching; wait for it
/// with [`Self::stream`].
///
/// [`AutostartPublisher::install`]: crate::AutostartPublisher::install
#[derive(Debug, Clone)]
pub struct LaunchReasonClient {
    slot: Arc<LatestValueSlot>,
}

impl LaunchReasonClient {
    /// Client bound to the process-wide runtime.
    ///
    /// # Errors
    ///
    /// Fails when no global runtime is installed or the plugin was not
    /// declared.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Client bound to `runtime`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and
    /// `istmo.autostart` was not declared.
    pub fn from_runtime(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(AUTOSTART_PLUGIN_ID)?;
        Ok(Self {
            slot: runtime.early_events().latest_slot(LAUNCH_CHANNEL),
        })
    }

    /// The published reason, `None` before the publish.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the payload does not decode.
    pub fn current(&self) -> Result<Option<LaunchReason>, IstmoError> {
        self.slot.peek().map(|bytes| decode(&bytes)).transpose()
    }

    /// The retained reason first (if any), then later publishes.
    #[must_use]
    pub fn stream(&self) -> LaunchReasonStream {
        LaunchReasonStream {
            rx: self.slot.subscribe(),
        }
    }
}

/// Decoding receiver of the [`LaunchReason`] lane.
#[derive(Debug)]
pub struct LaunchReasonStream {
    rx: Receiver<Vec<u8>>,
}

impl LaunchReasonStream {
    /// Block until the next value.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] once the runtime is gone,
    /// [`IstmoError::Codec`] when the payload does not decode.
    pub fn recv(&self) -> Result<LaunchReason, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Async variant of [`Self::recv`].
    ///
    /// # Errors
    ///
    /// Same as [`Self::recv`].
    pub async fn recv_async(&self) -> Result<LaunchReason, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Next buffered value without blocking.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<LaunchReason, IstmoError>> {
        self.rx.try_recv().ok().map(|bytes| decode(&bytes))
    }
}

fn decode(bytes: &[u8]) -> Result<LaunchReason, IstmoError> {
    Ok(codec::decode_exact_capped(bytes, MAX_REASON_BYTES)?)
}

/// Publish `reason` on the lane of `runtime`.
#[cfg_attr(
    any(
        target_os = "android",
        target_os = "ios",
        all(target_arch = "wasm32", target_os = "unknown")
    ),
    expect(dead_code, reason = "only the desktop publisher publishes")
)]
pub(crate) fn publish(runtime: &Runtime, reason: LaunchReason) {
    match codec::encode(&reason) {
        Ok(bytes) => runtime.publish_early_latest(LAUNCH_CHANNEL, bytes),
        Err(err) => tracing::warn!(target: "istmo.autostart", %err, "encode launch reason"),
    }
}
