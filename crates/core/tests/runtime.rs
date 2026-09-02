//! Runtime end-to-end behaviour driven by the mock backend.

use std::sync::Arc;

use flume::Receiver as FlumeReceiver;
use istmo_core::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, IstmoError, NativeHandle, NativeHandleId,
    PROTOCOL_VERSION, Runtime, StreamEndReason, StreamMessage,
};

fn mock() -> (Arc<Runtime>, FlumeReceiver<Envelope>) {
    let init = Runtime::mock();
    (init.runtime, init.outbound)
}

#[test]
fn call_round_trips_via_dispatch_inbound() {
    let (rt, outbound) = mock();
    let handle = rt.call("plugin", None, "add", vec![1, 2]).expect("call");
    let call_id = handle.call_id();

    let sent = outbound.recv().expect("outbound envelope");
    assert_eq!(sent.version, PROTOCOL_VERSION);
    match sent.frame {
        Frame::Call {
            call_id: c,
            plugin_id,
            method,
            payload,
            instance_id,
        } => {
            assert_eq!(c, call_id);
            assert_eq!(plugin_id, "plugin");
            assert_eq!(method, "add");
            assert_eq!(payload, vec![1, 2]);
            assert!(instance_id.is_none());
        }
        other => panic!("expected Call frame, got {other:?}"),
    }

    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Ok(vec![3]),
    }))
    .expect("dispatch");
    let result = pollster::block_on(handle).expect("call resolves");
    assert_eq!(result, Ok(vec![3]));
}

#[test]
fn dropping_a_pending_call_handle_sends_a_cancel_frame() {
    let (rt, outbound) = mock();
    let handle = rt.call("plugin", None, "slow", vec![]).expect("call");
    let call_id = handle.call_id();
    let call_frame = outbound.recv().expect("call frame");
    assert!(matches!(call_frame.frame, Frame::Call { .. }));
    drop(handle);
    let cancel = outbound.recv().expect("cancel frame");
    match cancel.frame {
        Frame::Cancel { call_id: c } => assert_eq!(c, call_id),
        other => panic!("expected Cancel, got {other:?}"),
    }
}

#[test]
fn resolved_call_handle_does_not_send_cancel_on_drop() {
    let (rt, outbound) = mock();
    let handle = rt.call("plugin", None, "quick", vec![]).expect("call");
    let call_id = handle.call_id();
    let _ = outbound.recv().expect("call frame");

    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Ok(vec![]),
    }))
    .expect("dispatch");
    let _ = pollster::block_on(handle).expect("resolves");
    // No further outbound envelopes must appear.
    assert!(outbound.try_recv().is_err());
}

#[test]
fn stream_delivers_events_and_terminal_end() {
    let (rt, outbound) = mock();
    let handle = rt
        .stream("plugin", None, "ticks", vec![], 8)
        .expect("stream");
    let stream_id = handle.stream_id();
    let _ = outbound.recv().expect("call frame");

    rt.dispatch_inbound(Envelope::new(Frame::Event {
        stream_id,
        payload: vec![1],
    }))
    .expect("ev1");
    rt.dispatch_inbound(Envelope::new(Frame::Event {
        stream_id,
        payload: vec![2],
    }))
    .expect("ev2");
    rt.dispatch_inbound(Envelope::new(Frame::StreamEnd {
        stream_id,
        reason: StreamEndReason::Complete,
    }))
    .expect("end");

    assert_eq!(handle.recv().unwrap(), StreamMessage::Event(vec![1]));
    assert_eq!(handle.recv().unwrap(), StreamMessage::Event(vec![2]));
    assert_eq!(
        handle.recv().unwrap(),
        StreamMessage::End(StreamEndReason::Complete)
    );
}

#[test]
fn dropping_a_stream_handle_sends_a_cancel_frame() {
    let (rt, outbound) = mock();
    let handle = rt
        .stream("plugin", None, "ticks", vec![], 4)
        .expect("stream");
    let stream_id = handle.stream_id();
    let _ = outbound.recv().expect("call frame");

    drop(handle);
    let cancel = outbound.recv().expect("cancel frame");
    match cancel.frame {
        Frame::Cancel { call_id } => assert_eq!(call_id.get(), stream_id.get()),
        other => panic!("expected Cancel, got {other:?}"),
    }
}

#[test]
fn create_and_destroy_instance_round_trip() {
    let (rt, outbound) = mock();
    let handle = rt.create_instance("plugin", vec![1, 2, 3]).expect("create");
    let call_id = handle.call_id();
    let sent = outbound.recv().expect("create frame");
    match sent.frame {
        Frame::CreateInstance {
            call_id: c,
            plugin_id,
            payload,
        } => {
            assert_eq!(c, call_id);
            assert_eq!(plugin_id, "plugin");
            assert_eq!(payload, vec![1, 2, 3]);
        }
        other => panic!("expected CreateInstance, got {other:?}"),
    }

    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: Ok(vec![99]),
    }))
    .expect("dispatch");
    let response = pollster::block_on(handle).expect("resolves");
    assert_eq!(response, Ok(vec![99]));

    let instance_id = InstanceId(99);
    rt.register_instance(instance_id, "plugin");
    assert_eq!(
        rt.routing().instance(instance_id).map(|e| e.plugin_id),
        Some("plugin".to_owned())
    );

    rt.destroy_instance(instance_id).expect("destroy");
    let sent = outbound.recv().expect("destroy frame");
    match sent.frame {
        Frame::DestroyInstance { instance_id: i } => assert_eq!(i, instance_id),
        other => panic!("expected DestroyInstance, got {other:?}"),
    }
    assert!(rt.routing().instance(instance_id).is_none());
}

#[test]
fn dispatch_inbound_rejects_a_protocol_version_mismatch() {
    let (rt, _outbound) = mock();
    let bad = Envelope {
        version: PROTOCOL_VERSION.wrapping_add(1),
        frame: Frame::Cancel { call_id: CallId(1) },
    };
    let err = rt.dispatch_inbound(bad).expect_err("mismatch");
    assert!(matches!(err, IstmoError::ProtocolVersionMismatch { .. }));
}

#[test]
fn shutdown_cancels_all_pending_calls() {
    let (rt, outbound) = mock();
    let h1 = rt.call("plugin", None, "a", vec![]).expect("call");
    let h2 = rt.call("plugin", None, "b", vec![]).expect("call");
    // Drain the two outbound Call frames so shutdown's channel is clean.
    let _ = outbound.recv().expect("call 1");
    let _ = outbound.recv().expect("call 2");

    rt.shutdown();

    // Both receivers must be closed on the caller side.
    let r1 = pollster::block_on(h1);
    let r2 = pollster::block_on(h2);
    assert!(matches!(r1, Err(IstmoError::ChannelClosed)));
    assert!(matches!(r2, Err(IstmoError::ChannelClosed)));
}

#[test]
fn ids_are_monotonic_across_call_and_stream() {
    let (rt, outbound) = mock();
    let c1 = rt.call("p", None, "a", vec![]).expect("c1");
    let s1 = rt.stream("p", None, "b", vec![], 1).expect("s1");
    let c2 = rt.call("p", None, "c", vec![]).expect("c2");
    // Drain outbound so nothing hangs.
    for _ in 0..3 {
        let _ = outbound.recv();
    }
    assert!(c1.call_id().get() < s1.stream_id().get());
    assert!(s1.stream_id().get() < c2.call_id().get());
    // Drop handles cleanly; they will each enqueue a Cancel that we ignore.
    drop((c1, s1, c2));
    for _ in 0..3 {
        let _ = outbound.recv();
    }
}

#[test]
fn dispatch_inbound_routes_early_event_latest_into_slot() {
    let (rt, _outbound) = mock();
    let rx = rt.early_events().latest_slot("istmo.lifecycle").subscribe();

    rt.dispatch_inbound(Envelope::new(Frame::EarlyEvent {
        channel: "istmo.lifecycle".to_owned(),
        kind: EarlyEventKind::Latest,
        payload: vec![7, 7, 7],
    }))
    .expect("dispatch");

    assert_eq!(rx.recv().expect("recv"), vec![7, 7, 7]);
}

#[test]
fn dispatch_inbound_routes_early_event_queue_into_queue() {
    let (rt, _outbound) = mock();
    // Two publishes before the first subscriber attaches. The queue must
    // buffer them because it was created with capacity 4 on the first
    // dispatch_inbound below (subsequent dispatches inherit the same queue).
    rt.dispatch_inbound(Envelope::new(Frame::EarlyEvent {
        channel: "istmo.deeplinks".to_owned(),
        kind: EarlyEventKind::Queue { capacity: 4 },
        payload: b"a".to_vec(),
    }))
    .expect("dispatch a");
    rt.dispatch_inbound(Envelope::new(Frame::EarlyEvent {
        channel: "istmo.deeplinks".to_owned(),
        kind: EarlyEventKind::Queue { capacity: 4 },
        payload: b"b".to_vec(),
    }))
    .expect("dispatch b");

    let rx = rt.early_events().queue("istmo.deeplinks", 4).subscribe();
    assert_eq!(rx.recv().expect("first"), b"a".to_vec());
    assert_eq!(rx.recv().expect("second"), b"b".to_vec());
}

/// Marker type used as the phantom parameter of the credential handle in
/// the tests below. No value is ever constructed.
struct GoogleCredential;

#[test]
fn release_native_handle_helper_emits_the_wire_frame() {
    let (rt, outbound) = mock();
    rt.release_native_handle(NativeHandleId(42))
        .expect("release");
    let env = outbound.recv().expect("frame");
    match env.frame {
        Frame::ReleaseNativeHandle { handle_id } => assert_eq!(handle_id, NativeHandleId(42)),
        other => panic!("expected ReleaseNativeHandle, got {other:?}"),
    }
}

#[test]
fn dropping_a_native_handle_sends_release_frame() {
    let (rt, outbound) = mock();
    let handle: NativeHandle<GoogleCredential> = NativeHandle::adopt(&rt, NativeHandleId(7));
    drop(handle);
    let env = outbound.recv().expect("release frame");
    match env.frame {
        Frame::ReleaseNativeHandle { handle_id } => assert_eq!(handle_id, NativeHandleId(7)),
        other => panic!("expected ReleaseNativeHandle, got {other:?}"),
    }
}

#[test]
fn into_id_suppresses_the_release_frame() {
    let (rt, outbound) = mock();
    let handle: NativeHandle<GoogleCredential> = NativeHandle::adopt(&rt, NativeHandleId(11));
    let id = handle.into_id();
    assert_eq!(id, NativeHandleId(11));
    assert!(
        outbound.try_recv().is_err(),
        "into_id must not enqueue a release"
    );
}

#[test]
fn stream_id_and_call_id_share_the_numeric_space() {
    let (rt, outbound) = mock();
    let s = rt.stream("p", None, "ticks", vec![], 4).expect("stream");
    let stream_id = s.stream_id();
    // The Call frame carries the same numeric id as the stream registration.
    let sent = outbound.recv().expect("call frame");
    match sent.frame {
        Frame::Call { call_id, .. } => assert_eq!(call_id.get(), stream_id.get()),
        other => panic!("expected Call, got {other:?}"),
    }
    // Drop the stream so we don't leak the cancel envelope.
    drop(s);
    let _ = outbound.recv();
}
