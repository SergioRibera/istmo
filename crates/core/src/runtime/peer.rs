//! Calls to the platform peer delivered on the caller's own thread.

use crate::protocol::{CallId, InstanceId, StreamId};
use crate::routing::CallResult;

/// A transport that hands a call to the peer (Kotlin, Swift) on the
/// thread making it, instead of queueing it for the transport's delivery
/// thread.
///
/// Installed with [`Runtime::install_direct_peer`](super::Runtime::install_direct_peer).
/// The runtime only asks it while no frame waits in the outbound channel
/// or is being delivered, so a call never overtakes a frame queued before
/// it; everything else (streams, cancels, notifications, releases) keeps
/// the outbound channel.
pub trait DirectPeer: Send + Sync + 'static {
    /// Deliver a [`Frame::Call`](crate::Frame::Call) now.
    ///
    /// Must not block beyond running the peer's handler up to its first
    /// suspension.
    fn call(
        &self,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: &str,
        payload: &[u8],
    ) -> DirectOutcome;

    /// Deliver one item of a stream the peer opened, now; `false` when
    /// it was not taken (the runtime queues it instead). The default
    /// takes nothing.
    fn event(&self, stream_id: StreamId, payload: &[u8]) -> bool {
        let _ = (stream_id, payload);
        false
    }
}

/// What became of a call handed to a [`DirectPeer`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum DirectOutcome {
    /// Answered before returning.
    Answered(CallResult),
    /// Taken; the answer arrives later as an inbound
    /// [`Frame::Respond`](crate::Frame::Respond) (it may already have,
    /// from inside the call).
    Pending,
    /// Not taken (this thread cannot reach the peer): the runtime sends
    /// the call through the outbound channel instead.
    Unavailable,
}
