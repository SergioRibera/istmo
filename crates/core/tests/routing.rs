//! Routing table behaviour: registration, delivery, cancellation and
//! type-mismatched delivery.

use istmo_core::{
    CallId, InstanceEntry, InstanceId, IstmoError, RoutingTables, StreamEndReason, StreamId,
    StreamMessage,
};

#[test]
fn register_and_deliver_call() {
    let table = RoutingTables::new();
    let rx = table.register_call(CallId(1));
    table
        .deliver_response(CallId(1), Ok(vec![7]))
        .expect("deliver");
    let result = rx.recv().expect("receiver alive");
    assert_eq!(result, Ok(vec![7]));
}

#[test]
fn delivering_to_unknown_call_id_returns_error() {
    let table = RoutingTables::new();
    let err = table
        .deliver_response(CallId(99), Ok(vec![]))
        .expect_err("unknown call id");
    assert!(matches!(err, IstmoError::UnknownCallId(id) if id == CallId(99)));
}

#[test]
fn register_and_deliver_stream() {
    let table = RoutingTables::new();
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
    // Sender was dropped after StreamEnd, so the next recv is a channel-closed error.
    assert!(rx.recv().is_err());
}

#[test]
fn stream_end_removes_the_registration() {
    let table = RoutingTables::new();
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
fn response_on_stream_registration_is_a_routing_mismatch() {
    let table = RoutingTables::new();
    let _rx = table.register_stream(StreamId(1), 0);
    let err = table
        .deliver_response(CallId(1), Ok(vec![]))
        .expect_err("routing mismatch");
    assert!(matches!(err, IstmoError::RoutingMismatch(_)));
}

#[test]
fn event_on_call_registration_is_a_routing_mismatch() {
    let table = RoutingTables::new();
    let _rx = table.register_call(CallId(1));
    let err = table
        .deliver_event(StreamId(1), vec![])
        .expect_err("routing mismatch");
    assert!(matches!(err, IstmoError::RoutingMismatch(_)));
}

#[test]
fn cancel_all_pending_clears_registrations() {
    let table = RoutingTables::new();
    let _c1 = table.register_call(CallId(1));
    let _c2 = table.register_call(CallId(2));
    let _s1 = table.register_stream(StreamId(3), 0);
    let removed = table.cancel_all_pending();
    assert_eq!(removed, 3);
    let err = table
        .deliver_response(CallId(1), Ok(vec![]))
        .expect_err("cleared");
    assert!(matches!(err, IstmoError::UnknownCallId(_)));
}

#[test]
fn instance_registry_round_trip() {
    let table = RoutingTables::new();
    table.register_instance(
        InstanceId(1),
        InstanceEntry {
            plugin_id: "com.example.p".to_owned(),
        },
    );
    let entry = table.instance(InstanceId(1)).expect("registered");
    assert_eq!(entry.plugin_id, "com.example.p");
    assert!(table.remove_instance(InstanceId(1)));
    assert!(table.instance(InstanceId(1)).is_none());
    assert!(!table.remove_instance(InstanceId(1)));
}
