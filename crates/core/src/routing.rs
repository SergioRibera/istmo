//! Routing tables mapping inbound frames back to the futures / streams that
//! originated them.
//!
//! Three logical registries live in [`RoutingTables`]:
//!
//! * `pending`: `u64 -> PendingReceiver`. Both calls and streams share this
//!   map because their numeric ids come from the same monotonic counter in
//!   the runtime — mixing them into a single table keeps the routing side
//!   O(1) with no fan-out logic, and the [`PendingReceiver`] variant records
//!   which kind was expected so an incorrectly-typed inbound frame produces
//!   a clean [`IstmoError::RoutingMismatch`] instead of silent misdelivery.
//! * `instances`: metadata for each live plugin instance, keyed by
//!   [`InstanceId`].
//!
//! The tables themselves are internally synchronised via [`std::sync::Mutex`].
//! Every locked section is a short hash-map mutation and never spans an
//! `await`, so a `std::sync` primitive is the right shape here even though
//! the surrounding runtime is async-facing.

use std::collections::HashMap;
use std::sync::Mutex;

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender};
use oneshot::Sender as OneshotSender;

use crate::error::IstmoError;
use crate::protocol::{CallId, InstanceId, StreamEndReason, StreamId};
use crate::sync::lock;

/// Result payload delivered by a successful (or errored) call.
pub type CallResult = Result<Vec<u8>, Vec<u8>>;

/// Messages delivered on the receiver side of a stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamMessage {
    Event(Vec<u8>),
    End(StreamEndReason),
}

/// Metadata stored per live native instance.
#[derive(Debug, Clone)]
pub struct InstanceEntry {
    pub plugin_id: String,
}

#[derive(Debug)]
enum PendingReceiver {
    Call(OneshotSender<CallResult>),
    Stream(FlumeSender<StreamMessage>),
}

/// All routing state for a single runtime.
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
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a pending call and returns the receiver end for the caller.
    #[must_use]
    pub fn register_call(&self, call_id: CallId) -> oneshot::Receiver<CallResult> {
        let (tx, rx) = oneshot::channel();
        lock(&self.inner)
            .pending
            .insert(call_id.get(), PendingReceiver::Call(tx));
        rx
    }

    /// Registers a stream and returns the receiver end for the caller.
    ///
    /// A `capacity` of zero requests an unbounded channel; any positive value
    /// is used as the bounded capacity. Bounded streams apply back-pressure to
    /// the sender: the runtime will park the caller if the receiver is slow.
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

    /// Removes a pending entry (call or stream) regardless of kind. Returns
    /// whether an entry was present.
    pub fn remove_pending(&self, id: u64) -> bool {
        lock(&self.inner).pending.remove(&id).is_some()
    }

    /// Peeks whether a pending entry exists for `id` without removing it.
    /// Used by same-runtime short-circuiting on `Respond` delivery.
    #[must_use]
    pub fn has_pending(&self, id: u64) -> bool {
        lock(&self.inner).pending.contains_key(&id)
    }

    /// Delivers a response to a previously registered call.
    pub fn deliver_response(&self, call_id: CallId, result: CallResult) -> Result<(), IstmoError> {
        let Some(entry) = lock(&self.inner).pending.remove(&call_id.get()) else {
            return Err(IstmoError::UnknownCallId(call_id));
        };
        match entry {
            PendingReceiver::Call(tx) => {
                // Receiver dropped means the caller no longer cares. Not an error.
                drop(tx.send(result));
                Ok(())
            }
            PendingReceiver::Stream(_) => Err(IstmoError::RoutingMismatch(
                "response landed on a stream registration",
            )),
        }
    }

    /// Delivers a stream event. The stream registration is preserved: further
    /// events for the same `stream_id` continue to route to the same receiver.
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

    /// Delivers the terminal `StreamEnd` for a stream, then removes it.
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

    /// Registers a newly created native instance.
    pub fn register_instance(&self, instance_id: InstanceId, entry: InstanceEntry) {
        lock(&self.inner).instances.insert(instance_id, entry);
    }

    /// Removes an instance registration. Returns whether it was present.
    pub fn remove_instance(&self, instance_id: InstanceId) -> bool {
        lock(&self.inner).instances.remove(&instance_id).is_some()
    }

    /// Copy of the instance entry for `instance_id`, if registered.
    #[must_use]
    pub fn instance(&self, instance_id: InstanceId) -> Option<InstanceEntry> {
        lock(&self.inner).instances.get(&instance_id).cloned()
    }

    /// Drops every pending entry: outstanding call receivers see the sender
    /// closed, and stream receivers see the sender closed as well. Instance
    /// entries are untouched. Returns the number of entries removed.
    pub fn cancel_all_pending(&self) -> usize {
        let mut inner = lock(&self.inner);
        let count = inner.pending.len();
        inner.pending.clear();
        count
    }
}
