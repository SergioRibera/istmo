//! `LiveActivity` plugin: wire round-trip against a mock backend, plus
//! [`TypedLiveActivity`] encode/decode + domain-error surfacing.

use std::sync::Arc;
use std::thread;

use istmo_core::{Envelope, Frame, NativeHandleId, Runtime, codec};
use istmo_live_activity::{
    ActivityError, ActivityStyle, DismissalPolicy, LIVE_ACTIVITY_PLUGIN_ID, LiveActivityClient,
    TypedLiveActivity,
};
use istmo_macros::message;

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq)]
struct TimerAttributes {
    title: String,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq)]
struct TimerState {
    elapsed: u64,
}

/// Backend thread that services exactly one `Call` for `method` and
/// responds with `respond`.
fn spawn_backend(
    rt: Arc<Runtime>,
    outbound: flume::Receiver<Envelope>,
    method: &'static str,
    respond: Result<Vec<u8>, Vec<u8>>,
) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let env = outbound.recv().expect("call envelope");
        let (call_id, payload) = match env.frame {
            Frame::Call {
                call_id,
                plugin_id,
                method: m,
                payload,
                instance_id,
            } => {
                assert_eq!(plugin_id, LIVE_ACTIVITY_PLUGIN_ID);
                assert!(instance_id.is_none(), "live-activity is a stateless plugin");
                assert_eq!(m, method);
                (call_id, payload)
            }
            other => panic!("expected Call, got {other:?}"),
        };
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id,
            result: respond,
        }))
        .unwrap();
        payload
    })
}

#[test]
fn typed_start_encodes_payload_and_returns_handle_id() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let handle_id = NativeHandleId(777);
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "start",
        Ok(codec::encode(&handle_id).unwrap()),
    );

    let activities =
        TypedLiveActivity::<TimerAttributes, TimerState>::new(&rt, "timer").expect("bind");
    let handle = pollster::block_on(activities.start(
        TimerAttributes { title: "Focus".into() },
        TimerState { elapsed: 0 },
        ActivityStyle::Standard,
    ))
    .expect("start ok");

    // Consume the handle without firing a release — the test does not run
    // a receive-side release handler.
    let issued = handle.into_id();
    assert_eq!(issued, handle_id);

    let payload = backend.join().unwrap();
    // Decode the outbound tuple `(activity_type, attributes, initial_state,
    // style, stale_after_seconds, android_tier_hint)` — client codegen keeps
    // this shape in argument order.
    type StartTuple = (
        String,
        Vec<u8>,
        Vec<u8>,
        ActivityStyle,
        Option<u32>,
        Option<istmo_live_activity::AndroidTierHint>,
    );
    let ((activity_type, attrs_bytes, state_bytes, style, stale, hint), _) =
        codec::decode::<StartTuple>(&payload).unwrap();
    assert_eq!(activity_type, "timer");
    assert_eq!(style, ActivityStyle::Standard);
    assert!(stale.is_none());
    assert!(hint.is_none());

    let (attrs, _) = codec::decode::<TimerAttributes>(&attrs_bytes).unwrap();
    assert_eq!(attrs.title, "Focus");
    let (state, _) = codec::decode::<TimerState>(&state_bytes).unwrap();
    assert_eq!(state.elapsed, 0);
}

#[test]
fn typed_update_encodes_new_state() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let ok_bytes = codec::encode(&()).unwrap();
    let backend = spawn_backend(rt.clone(), init.outbound, "update", Ok(ok_bytes));

    let activities =
        TypedLiveActivity::<TimerAttributes, TimerState>::new(&rt, "timer").expect("bind");
    pollster::block_on(activities.update(NativeHandleId(42), TimerState { elapsed: 30 }, None))
        .expect("update ok");

    let payload = backend.join().unwrap();
    type UpdateTuple = (
        NativeHandleId,
        Vec<u8>,
        Option<istmo_live_activity::AlertConfig>,
    );
    let ((handle, state_bytes, alert), _) = codec::decode::<UpdateTuple>(&payload).unwrap();
    assert_eq!(handle, NativeHandleId(42));
    assert!(alert.is_none());
    let (state, _) = codec::decode::<TimerState>(&state_bytes).unwrap();
    assert_eq!(state.elapsed, 30);
}

#[test]
fn typed_end_forwards_dismissal_policy() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let ok_bytes = codec::encode(&()).unwrap();
    let backend = spawn_backend(rt.clone(), init.outbound, "end", Ok(ok_bytes));

    let activities =
        TypedLiveActivity::<TimerAttributes, TimerState>::new(&rt, "timer").expect("bind");
    pollster::block_on(activities.end(
        NativeHandleId(9),
        Some(TimerState { elapsed: 300 }),
        DismissalPolicy::AfterSeconds(10),
    ))
    .expect("end ok");

    let payload = backend.join().unwrap();
    type EndTuple = (NativeHandleId, Option<Vec<u8>>, DismissalPolicy);
    let ((handle, final_bytes, dismissal), _) = codec::decode::<EndTuple>(&payload).unwrap();
    assert_eq!(handle, NativeHandleId(9));
    assert_eq!(dismissal, DismissalPolicy::AfterSeconds(10));
    let (state, _) = codec::decode::<TimerState>(final_bytes.as_ref().unwrap()).unwrap();
    assert_eq!(state.elapsed, 300);
}

#[test]
fn plugin_error_bytes_decode_into_activity_error() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let err = ActivityError::UnknownActivityType("timer".into());
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "start",
        Err(codec::encode(&err).unwrap()),
    );

    let activities =
        TypedLiveActivity::<TimerAttributes, TimerState>::new(&rt, "timer").expect("bind");
    let result = pollster::block_on(activities.start(
        TimerAttributes { title: "x".into() },
        TimerState { elapsed: 0 },
        ActivityStyle::Standard,
    ));

    match result.expect_err("domain error surfaces") {
        ActivityError::UnknownActivityType(ty) => assert_eq!(ty, "timer"),
        other => panic!("expected UnknownActivityType, got {other:?}"),
    }

    backend.join().unwrap();
}

#[test]
fn base_client_capabilities_round_trips() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let caps = istmo_live_activity::PlatformCapabilities::Android(
        istmo_live_activity::AndroidCapabilities {
            supports_custom: true,
            supports_progress_style: false,
            supports_live_update: false,
            notifications_enabled: true,
        },
    );
    let backend = spawn_backend(
        rt.clone(),
        init.outbound,
        "capabilities",
        Ok(codec::encode(&caps).unwrap()),
    );

    let client = LiveActivityClient::from_runtime(&rt).expect("bind");
    let value = pollster::block_on(client.capabilities()).expect("capabilities ok");
    assert_eq!(value, caps);
    backend.join().unwrap();
}
