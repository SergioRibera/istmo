//! Pending-call, stream and instance registries used by the runtime.

use std::collections::HashMap;
use std::sync::Mutex;

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender};
use oneshot::Sender as OneshotSender;

use crate::error::IstmoError;
use crate::protocol::{CallId, InstanceId, StreamEndReason, StreamId};
use crate::sync::lock;

/// Bincode-encoded outcome of a unary call.
///
/// `Ok(bytes)` carries a decoded return value; `Err(bytes)` carries a
/// plugin-declared domain error.
pub type CallResult = Result<Vec<u8>, Vec<u8>>;

/// One decoded frame off a live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamMessage {
    /// A live event carrying a bincode-encoded item.
    Event(Vec<u8>),
    /// Stream terminated.
    End(StreamEndReason),
}

/// Bookkeeping entry for a live plugin instance.
#[derive(Debug, Clone)]
pub struct InstanceEntry {
    /// Plugin id the instance belongs to.
    pub plugin_id: String,
}

#[derive(Debug)]
enum PendingReceiver {
    Call(OneshotSender<CallResult>),
    Stream(FlumeSender<StreamMessage>),
}

/// In-memory registry pairing pending [`CallId`]s / [`StreamId`]s with
/// the channels waiting on their completion.
///
/// Owned by the runtime; the transport crates never touch these tables
/// directly.
#[derive(Debug, Default)]
pub struct RoutingTables {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    pending: HashMap<u64, PendingReceiver>,
    instances: HashMap<InstanceId, InstanceEntry>,
}

impl RoutingTables {
    /// Build an empty set of routing tables.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a pending call and return its response receiver.
    #[must_use]
    pub fn register_call(&self, call_id: CallId) -> oneshot::Receiver<CallResult> {
        let (tx, rx) = oneshot::channel();
        lock(&self.inner)
            .pending
            .insert(call_id.get(), PendingReceiver::Call(tx));
        rx
    }

    /// Register a pending stream and return its event receiver.
    ///
    /// `capacity == 0` yields an unbounded channel; any other value
    /// caps the buffer at `capacity` messages before the producer
    /// blocks.
    #[must_use]
    pub fn register_stream(
        &self,
        stream_id: StreamId,
        capacity: usize,
    ) -> FlumeReceiver<StreamMessage> {
        let (tx, rx) = if capacity == 0 {
            flume::unbounded()
        } else {
            flume::bounded(capacity)
        };
        lock(&self.inner)
            .pending
            .insert(stream_id.get(), PendingReceiver::Stream(tx));
        rx
    }

    /// Drop the registration matching `id`. Returns `true` if it was
    /// present.
    pub fn remove_pending(&self, id: u64) -> bool {
        lock(&self.inner).pending.remove(&id).is_some()
    }

    /// Return `true` if `id` currently has a pending call or stream
    /// waiting on it.
    #[must_use]
    pub fn has_pending(&self, id: u64) -> bool {
        lock(&self.inner).pending.contains_key(&id)
    }

    /// Deliver a unary response and close the matching registration.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownCallId`] if no registration exists.
    /// - [`IstmoError::RoutingMismatch`] if the id was registered as a
    ///   stream.
    pub fn deliver_response(&self, call_id: CallId, result: CallResult) -> Result<(), IstmoError> {
        let Some(entry) = lock(&self.inner).pending.remove(&call_id.get()) else {
            return Err(IstmoError::UnknownCallId(call_id));
        };
        match entry {
            PendingReceiver::Call(tx) => {

                drop(tx.send(result));
                Ok(())
            }
            PendingReceiver::Stream(_) => Err(IstmoError::RoutingMismatch(
                "response landed on a stream registration",
            )),
        }
    }

    /// Push a stream event into the matching registration.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownStreamId`] if no registration exists.
    /// - [`IstmoError::RoutingMismatch`] if the id was registered as a
    ///   unary call.
    /// - [`IstmoError::ChannelClosed`] if the consumer's receiver has
    ///   been dropped.
    pub fn deliver_event(&self, stream_id: StreamId, payload: Vec<u8>) -> Result<(), IstmoError> {
        let sender = {
            let inner = lock(&self.inner);
            match inner.pending.get(&stream_id.get()) {
                Some(PendingReceiver::Stream(tx)) => tx.clone(),
                Some(PendingReceiver::Call(_)) => {
                    return Err(IstmoError::RoutingMismatch(
                        "event landed on a call registration",
                    ));
                }
                None => return Err(IstmoError::UnknownStreamId(stream_id)),
            }
        };
        sender
            .send(StreamMessage::Event(payload))
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Close a stream registration with the given `reason`.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownStreamId`] if the stream isn't
    ///   registered.
    /// - [`IstmoError::RoutingMismatch`] if the id was registered as a
    ///   unary call.
    pub fn deliver_stream_end(
        &self,
        stream_id: StreamId,
        reason: StreamEndReason,
    ) -> Result<(), IstmoError> {
        let Some(entry) = lock(&self.inner).pending.remove(&stream_id.get()) else {
            return Err(IstmoError::UnknownStreamId(stream_id));
        };
        match entry {
            PendingReceiver::Stream(tx) => {
                drop(tx.send(StreamMessage::End(reason)));
                Ok(())
            }
            PendingReceiver::Call(_) => Err(IstmoError::RoutingMismatch(
                "stream-end landed on a call registration",
            )),
        }
    }

    /// Record a live plugin instance under `instance_id`.
    pub fn register_instance(&self, instance_id: InstanceId, entry: InstanceEntry) {
        lock(&self.inner).instances.insert(instance_id, entry);
    }

    /// Drop the registration for `instance_id`. Returns `true` if it
    /// was present.
    pub fn remove_instance(&self, instance_id: InstanceId) -> bool {
        lock(&self.inner).instances.remove(&instance_id).is_some()
    }

    /// Look up an instance registration.
    #[must_use]
    pub fn instance(&self, instance_id: InstanceId) -> Option<InstanceEntry> {
        lock(&self.inner).instances.get(&instance_id).cloned()
    }

    /// Drop every pending call and stream registration.
    ///
    /// Returns the number of entries cleared. Used during runtime
    /// shutdown to wake blocked receivers.
    pub fn cancel_all_pending(&self) -> usize {
        let mut inner = lock(&self.inner);
        let count = inner.pending.len();
        inner.pending.clear();
        count
    }
}

