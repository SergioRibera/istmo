//! Wire round-trip tests: every `Frame` variant encodes and decodes to the
//! same value under the standard codec configuration.

use istmo_core::{
    CallId, CodecError, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId,
    PROTOCOL_VERSION, StreamEndReason, StreamId, codec,
};

fn all_frames() -> Vec<Frame> {
    vec![
        Frame::Call {
            call_id: CallId(1),
            plugin_id: "com.example.plug".to_owned(),
            instance_id: None,
            method: "ping".to_owned(),
            payload: vec![1, 2, 3],
        },
        Frame::Call {
            call_id: CallId(2),
            plugin_id: "com.example.plug".to_owned(),
            instance_id: Some(InstanceId(42)),
            method: "with_instance".to_owned(),
            payload: vec![],
        },
        Frame::Respond {
            call_id: CallId(3),
            result: Ok(vec![9, 8, 7]),
        },
        Frame::Respond {
            call_id: CallId(4),
            result: Err(vec![0xff, 0xfe]),
        },
        Frame::Cancel { call_id: CallId(5) },
        Frame::Event {
            stream_id: StreamId(6),
            payload: vec![],
        },
        Frame::StreamEnd {
            stream_id: StreamId(6),
            reason: StreamEndReason::Complete,
        },
        Frame::StreamEnd {
            stream_id: StreamId(7),
            reason: StreamEndReason::Cancelled,
        },
        Frame::StreamEnd {
            stream_id: StreamId(8),
            reason: StreamEndReason::Error(vec![42, 43]),
        },
        Frame::CreateInstance {
            call_id: CallId(9),
            plugin_id: "com.example.plug".to_owned(),
            payload: vec![1],
        },
        Frame::DestroyInstance {
            instance_id: InstanceId(9),
        },
        Frame::EarlyEvent {
            channel: "istmo.lifecycle".to_owned(),
            kind: EarlyEventKind::Latest,
            payload: vec![0xa5],
        },
        Frame::EarlyEvent {
            channel: "istmo.deeplinks".to_owned(),
            kind: EarlyEventKind::Queue { capacity: 16 },
            payload: b"scheme://foo".to_vec(),
        },
        Frame::ReleaseNativeHandle {
            handle_id: NativeHandleId(999),
        },
        Frame::Notify {
            plugin_id: "com.example.notify".to_owned(),
            instance_id: None,
            method: "release".to_owned(),
            payload: vec![0xde, 0xad, 0xbe, 0xef],
        },
        Frame::Notify {
            plugin_id: "com.example.notify".to_owned(),
            instance_id: Some(InstanceId(7)),
            method: "instance_release".to_owned(),
            payload: vec![],
        },
    ]
}

#[test]
fn envelope_round_trips_for_every_frame_variant() {
    for frame in all_frames() {
        let env = Envelope::new(frame.clone());
        let bytes = codec::encode(&env).expect("encode");
        let (decoded, consumed) = codec::decode::<Envelope>(&bytes).expect("decode");
        assert_eq!(decoded, env, "round trip mismatch for {frame:?}");
        assert_eq!(
            consumed,
            bytes.len(),
            "codec should consume every byte of a single envelope"
        );
        assert_eq!(decoded.version, PROTOCOL_VERSION);
    }
}

#[test]
fn corrupted_bytes_produce_a_decode_error() {
    let err = codec::decode::<Envelope>(&[0xff; 4]).expect_err("garbage should not decode");
    assert!(matches!(err, CodecError::Decode(_)));
}

#[test]
fn extra_trailing_bytes_are_reported_by_the_consumed_count() {
    let env = Envelope::new(Frame::Cancel { call_id: CallId(1) });
    let mut bytes = codec::encode(&env).expect("encode");
    let original_len = bytes.len();
    bytes.extend_from_slice(&[0, 0, 0]);
    let (decoded, consumed) = codec::decode::<Envelope>(&bytes).expect("decode");
    assert_eq!(decoded, env);
    assert_eq!(consumed, original_len);
    assert_eq!(bytes.len() - consumed, 3);
}
