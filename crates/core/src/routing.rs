//! Pending-call, stream and instance registries used by the runtime.
//!
//! The registries themselves are internal to the runtime; this module
//! exposes the value types that cross its public surface
//! ([`CallResult`], [`StreamMessage`]).

use std::collections::HashMap;
use std::sync::Mutex;

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender};
use oneshot::Sender as OneshotSender;

use crate::error::IstmoError;
use crate::protocol::{CallId, InstanceId, StreamEndReason, StreamId};
use crate::sync::lock;

/// Wire-encoded outcome of a unary call.
///
/// `Ok(bytes)` carries a decoded return value; `Err(bytes)` carries a
/// plugin-declared domain error.
pub type CallResult = Result<Vec<u8>, Vec<u8>>;

/// One decoded frame off a live stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamMessage {
    /// A live event carrying a wire-encoded item.
    Event(Vec<u8>),
    /// Stream terminated.
    End(StreamEndReason),
}

#[derive(Debug)]
enum PendingReceiver {
    Call(OneshotSender<CallResult>),
    Stream(FlumeSender<StreamMessage>),
}

/// In-memory registry pairing pending [`CallId`]s / [`StreamId`]s with
/// the channels waiting on their completion, plus the plugin owning
/// each locally hosted instance.
#[derive(Debug, Default)]
pub(crate) struct RoutingTables {
    inner: Mutex<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    pending: HashMap<u64, PendingReceiver>,
    /// Instance id -> plugin id.
    instances: HashMap<InstanceId, String>,
}

impl RoutingTables {
    /// Register a pending call and return its response receiver.
    #[must_use]
    pub(crate) fn register_call(&self, call_id: CallId) -> oneshot::Receiver<CallResult> {
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
    pub(crate) fn register_stream(
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
    pub(crate) fn remove_pending(&self, id: u64) -> bool {
        lock(&self.inner).pending.remove(&id).is_some()
    }

    /// Deliver a unary response and close the matching registration.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownCallId`] if no registration exists.
    /// - [`IstmoError::RoutingMismatch`] if the id was registered as a
    ///   stream; the stream registration is left untouched.
    pub(crate) fn deliver_response(
        &self,
        call_id: CallId,
        result: CallResult,
    ) -> Result<(), IstmoError> {
        let mut inner = lock(&self.inner);
        // Peek before removing: a stray Respond carrying a live stream's
        // id must not tear down that stream's registration.
        match inner.pending.get(&call_id.get()) {
            None => return Err(IstmoError::UnknownCallId(call_id)),
            Some(PendingReceiver::Stream(_)) => {
                return Err(IstmoError::RoutingMismatch(
                    "response landed on a stream registration",
                ));
            }
            Some(PendingReceiver::Call(_)) => {}
        }
        let entry = inner.pending.remove(&call_id.get());
        drop(inner);
        if let Some(PendingReceiver::Call(tx)) = entry {
            drop(tx.send(result));
        }
        Ok(())
    }

    /// Finish a locally issued hosted call with its dispatch result,
    /// whichever kind of registration is waiting.
    ///
    /// A stream method whose dispatch failed produces a unary result;
    /// its stream registration then ends with
    /// [`StreamEndReason::Error`] carrying the error bytes instead of
    /// waiting forever.
    ///
    /// # Errors
    ///
    /// [`IstmoError::UnknownCallId`] if no registration exists.
    pub(crate) fn complete(&self, call_id: CallId, result: CallResult) -> Result<(), IstmoError> {
        let entry = lock(&self.inner).pending.remove(&call_id.get());
        match entry {
            None => Err(IstmoError::UnknownCallId(call_id)),
            Some(PendingReceiver::Call(tx)) => {
                drop(tx.send(result));
                Ok(())
            }
            Some(PendingReceiver::Stream(tx)) => {
                let reason = match result {
                    Ok(_) => StreamEndReason::Complete,
                    Err(bytes) => StreamEndReason::Error(bytes),
                };
                drop(tx.send(StreamMessage::End(reason)));
                Ok(())
            }
        }
    }

    /// Push a stream event into the matching registration.
    ///
    /// Blocks while a bounded stream's buffer is full: that is the
    /// backpressure [`Runtime::stream`](crate::Runtime::stream)'s
    /// `capacity` promises, and it lands on whichever thread delivers
    /// the event (the local producer pump, or the platform thread that
    /// submitted the inbound frame). A transport that multiplexes many
    /// streams over one thread should open them with `capacity == 0`.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownStreamId`] if no registration exists.
    /// - [`IstmoError::RoutingMismatch`] if the id was registered as a
    ///   unary call.
    /// - [`IstmoError::ChannelClosed`] if the consumer's receiver has
    ///   been dropped.
    pub(crate) fn deliver_event(
        &self,
        stream_id: StreamId,
        payload: Vec<u8>,
    ) -> Result<(), IstmoError> {
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
    ///   unary call; the call registration is left untouched.
    pub(crate) fn deliver_stream_end(
        &self,
        stream_id: StreamId,
        reason: StreamEndReason,
    ) -> Result<(), IstmoError> {
        let mut inner = lock(&self.inner);
        match inner.pending.get(&stream_id.get()) {
            None => return Err(IstmoError::UnknownStreamId(stream_id)),
            Some(PendingReceiver::Call(_)) => {
                return Err(IstmoError::RoutingMismatch(
                    "stream-end landed on a call registration",
                ));
            }
            Some(PendingReceiver::Stream(_)) => {}
        }
        let entry = inner.pending.remove(&stream_id.get());
        drop(inner);
        if let Some(PendingReceiver::Stream(tx)) = entry {
            drop(tx.send(StreamMessage::End(reason)));
        }
        Ok(())
    }

    /// Record that `instance_id` belongs to `plugin_id`.
    pub(crate) fn register_instance(&self, instance_id: InstanceId, plugin_id: String) {
        lock(&self.inner).instances.insert(instance_id, plugin_id);
    }

    /// Drop the registration for `instance_id`, returning the plugin it
    /// belonged to.
    pub(crate) fn remove_instance(&self, instance_id: InstanceId) -> Option<String> {
        lock(&self.inner).instances.remove(&instance_id)
    }

    /// Plugin owning `instance_id`, if registered.
    #[must_use]
    pub(crate) fn instance(&self, instance_id: InstanceId) -> Option<String> {
        lock(&self.inner).instances.get(&instance_id).cloned()
    }

    /// Drop every pending call and stream registration plus every
    /// instance registration.
    ///
    /// Returns the number of pending entries cleared. Dropping the
    /// senders wakes blocked receivers with a closed-channel error.
    pub(crate) fn clear(&self) -> usize {
        let mut inner = lock(&self.inner);
        let count = inner.pending.len();
        inner.pending.clear();
        inner.instances.clear();
        count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn register_and_deliver_call() {
        let table = RoutingTables::default();
        let rx = table.register_call(CallId(1));
        table
            .deliver_response(CallId(1), Ok(vec![7]))
            .expect("deliver");
        assert_eq!(rx.recv().expect("receiver alive"), Ok(vec![7]));
    }

    #[test]
    fn delivering_to_unknown_call_id_returns_error() {
        let table = RoutingTables::default();
        let err = table
            .deliver_response(CallId(99), Ok(vec![]))
            .expect_err("unknown call id");
        assert!(matches!(err, IstmoError::UnknownCallId(id) if id == CallId(99)));
    }

    #[test]
    fn register_and_deliver_stream() {
        let table = RoutingTables::default();
        let rx = table.register_stream(StreamId(1), 4);
        table.deliver_event(StreamId(1), vec![1]).expect("ev1");
        table.deliver_event(StreamId(1), vec![2]).expect("ev2");
        table
            .deliver_stream_end(StreamId(1), StreamEndReason::Complete)
            .expect("end");
        assert_eq!(rx.recv().unwrap(), StreamMessage::Event(vec![1]));
        assert_eq!(rx.recv().unwrap(), StreamMessage::Event(vec![2]));
        assert_eq!(
            rx.recv().unwrap(),
            StreamMessage::End(StreamEndReason::Complete)
        );
        assert!(rx.recv().is_err());
    }

    #[test]
    fn stream_end_removes_the_registration() {
        let table = RoutingTables::default();
        let _rx = table.register_stream(StreamId(1), 0);
        table
            .deliver_stream_end(StreamId(1), StreamEndReason::Cancelled)
            .expect("end");
        let err = table
            .deliver_event(StreamId(1), vec![])
            .expect_err("stream should no longer be registered");
        assert!(matches!(err, IstmoError::UnknownStreamId(id) if id == StreamId(1)));
    }

    #[test]
    fn response_on_stream_registration_keeps_the_stream() {
        let table = RoutingTables::default();
        let rx = table.register_stream(StreamId(1), 0);
        let err = table
            .deliver_response(CallId(1), Ok(vec![]))
            .expect_err("routing mismatch");
        assert!(matches!(err, IstmoError::RoutingMismatch(_)));
        table
            .deliver_event(StreamId(1), vec![3])
            .expect("still live");
        assert_eq!(rx.recv().unwrap(), StreamMessage::Event(vec![3]));
    }

    #[test]
    fn stream_end_on_call_registration_keeps_the_call() {
        let table = RoutingTables::default();
        let rx = table.register_call(CallId(1));
        let err = table
            .deliver_stream_end(StreamId(1), StreamEndReason::Complete)
            .expect_err("routing mismatch");
        assert!(matches!(err, IstmoError::RoutingMismatch(_)));
        table
            .deliver_response(CallId(1), Ok(vec![4]))
            .expect("still live");
        assert_eq!(rx.recv().unwrap(), Ok(vec![4]));
    }

    #[test]
    fn event_on_call_registration_is_a_routing_mismatch() {
        let table = RoutingTables::default();
        let _rx = table.register_call(CallId(1));
        let err = table
            .deliver_event(StreamId(1), vec![])
            .expect_err("routing mismatch");
        assert!(matches!(err, IstmoError::RoutingMismatch(_)));
    }

    #[test]
    fn clear_drops_every_registration() {
        let table = RoutingTables::default();
        let _c1 = table.register_call(CallId(1));
        let _c2 = table.register_call(CallId(2));
        let _s1 = table.register_stream(StreamId(3), 0);
        table.register_instance(InstanceId(4), "p".to_owned());
        assert_eq!(table.clear(), 3);
        let err = table
            .deliver_response(CallId(1), Ok(vec![]))
            .expect_err("cleared");
        assert!(matches!(err, IstmoError::UnknownCallId(_)));
        assert!(table.instance(InstanceId(4)).is_none());
    }

    #[test]
    fn instance_registry_round_trip() {
        let table = RoutingTables::default();
        table.register_instance(InstanceId(1), "com.example.p".to_owned());
        assert_eq!(
            table.instance(InstanceId(1)).as_deref(),
            Some("com.example.p")
        );
        assert_eq!(
            table.remove_instance(InstanceId(1)).as_deref(),
            Some("com.example.p")
        );
        assert!(table.instance(InstanceId(1)).is_none());
        assert!(table.remove_instance(InstanceId(1)).is_none());
    }
}
