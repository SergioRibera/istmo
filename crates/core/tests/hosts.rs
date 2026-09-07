//! Hosted plugin dispatch: inbound `Frame::Call` → registered dispatcher →
//! `Frame::Respond` back on the outbound channel.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, Envelope, Frame, InstanceId, Outcome, Runtime,
    codec,
};

/// Trivial dispatcher: replies with the request payload prefixed by the
/// method byte length.
struct EchoDispatch {
    calls: AtomicU32,
    slow: AtomicBool,
}

impl EchoDispatch {
    const PLUGIN_ID: &'static str = "test.echo";

    const fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
            slow: AtomicBool::new(false),
        }
    }
}

impl Dispatch for EchoDispatch {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let method_owned = method.to_owned();
        let payload_owned = payload.to_vec();
        let slow = self.slow.load(Ordering::SeqCst);
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if slow {
                std::thread::sleep(Duration::from_millis(80));
            }
            let mut out = Vec::new();
            out.extend_from_slice(method_owned.as_bytes());
            out.push(b':');
            out.extend_from_slice(&payload_owned);
            Ok(Outcome::Ok(out))
        })
    }
}

fn wait_for_respond(outbound: &flume::Receiver<Envelope>, call_id: CallId) -> Frame {
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if let Ok(env) = outbound.recv_timeout(Duration::from_millis(50)) {
            if let Frame::Respond { call_id: cid, .. } = &env.frame {
                if *cid == call_id {
                    return env.frame;
                }
            }
        }
    }
    panic!("timed out waiting for Respond for {call_id:?}");
}

#[test]
fn inbound_call_routes_to_registered_host() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let dispatcher = EchoDispatch::new();
    rt.register_host(dispatcher);

    let call_id = CallId(101);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: EchoDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "ping".to_owned(),
        payload: b"hello".to_vec(),
    }))
    .expect("dispatch call");

    let frame = wait_for_respond(&outbound, call_id);
    match frame {
        Frame::Respond {
            result: Ok(bytes), ..
        } => assert_eq!(bytes, b"ping:hello".to_vec()),
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[test]
fn inbound_call_for_unknown_plugin_replies_with_error() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let call_id = CallId(202);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "not.registered".to_owned(),
        instance_id: None,
        method: "any".to_owned(),
        payload: vec![],
    }))
    .expect("dispatch call");

    let frame = wait_for_respond(&outbound, call_id);
    match frame {
        Frame::Respond {
            result: Err(bytes), ..
        } => {
            let (msg, _) = codec::decode::<String>(&bytes).expect("decode error");
            assert!(msg.contains("not.registered"), "got: {msg}");
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[test]
fn inbound_cancel_before_completion_drops_the_response() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let dispatcher = EchoDispatch::new();
    dispatcher.slow.store(true, Ordering::SeqCst);
    rt.register_host(dispatcher);

    let call_id = CallId(303);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: EchoDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "slow".to_owned(),
        payload: vec![],
    }))
    .expect("dispatch call");

    // Cancel before the 80ms sleep completes.
    std::thread::sleep(Duration::from_millis(10));
    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .expect("dispatch cancel");

    // No Respond should ever arrive.
    let result = outbound.recv_timeout(Duration::from_millis(200));
    assert!(
        result.is_err(),
        "expected no envelope after cancel; got {result:?}",
    );
}

#[test]
fn declare_plugin_marks_ids_visible() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;

    assert!(!rt.is_plugin_declared("istmo.permissions"));
    rt.declare_plugin("istmo.permissions");
    assert!(rt.is_plugin_declared("istmo.permissions"));
    assert!(!rt.is_plugin_declared("istmo.other"));
}

#[test]
fn check_declared_is_permissive_by_default() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    // Enforcement off — anything passes even without declaration.
    rt.check_declared("test.anything").expect("permissive");
}

#[test]
fn check_declared_rejects_undeclared_when_enforcing() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    rt.declare_plugin("test.declared");
    rt.set_enforce_declarations(true);

    rt.check_declared("test.declared").expect("declared ok");
    let err = rt
        .check_declared("test.other")
        .expect_err("must reject undeclared");
    assert!(
        matches!(err, istmo_core::IstmoError::PluginNotDeclared("test.other")),
        "expected PluginNotDeclared, got {err:?}",
    );
}

/// Dispatcher that polls its cancel token between short sleeps. Records
/// whether it observed cancellation before completion.
struct CooperativeDispatch {
    saw_cancel: Arc<AtomicBool>,
}

impl CooperativeDispatch {
    const PLUGIN_ID: &'static str = "test.coop";
}

impl Dispatch for CooperativeDispatch {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let saw = Arc::clone(&self.saw_cancel);
        Box::pin(async move {
            for _ in 0..50 {
                if cancel.is_cancelled() {
                    saw.store(true, Ordering::SeqCst);
                    return Ok(Outcome::Ok(b"cancelled".to_vec()));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(Outcome::Ok(b"completed".to_vec()))
        })
    }
}

#[test]
fn cooperative_cancel_token_trips_dispatcher_mid_flight() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let saw_cancel = Arc::new(AtomicBool::new(false));
    rt.register_host(CooperativeDispatch {
        saw_cancel: Arc::clone(&saw_cancel),
    });

    let call_id = CallId(404);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: CooperativeDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "wait".to_owned(),
        payload: vec![],
    }))
    .expect("dispatch call");

    // Let the dispatcher enter its polling loop, then cancel.
    std::thread::sleep(Duration::from_millis(20));
    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .expect("dispatch cancel");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !saw_cancel.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "dispatcher never observed cancel token",
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    // Response is still discarded — the dispatcher short-circuits, and the
    // runtime drops any reply arriving after Cancel was recorded.
    let result = outbound.recv_timeout(Duration::from_millis(200));
    assert!(
        result.is_err(),
        "expected no envelope after cancel; got {result:?}",
    );
}

#[test]
fn cancel_arriving_before_dispatch_starts_trips_token_immediately() {
    // Dispatch a call whose dispatcher records the token state on entry;
    // pre-cancel by inserting Cancel first via a hand-built pre-populated
    // dispatcher setup.
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let saw_cancel = Arc::new(AtomicBool::new(false));
    rt.register_host(CooperativeDispatch {
        saw_cancel: Arc::clone(&saw_cancel),
    });

    let call_id = CallId(505);
    // Cancel first — no active call yet. Runtime should pre-insert a
    // cancelled token keyed by this call id.
    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .expect("pre-cancel");
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: CooperativeDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "wait".to_owned(),
        payload: vec![],
    }))
    .expect("dispatch call");

    let deadline = std::time::Instant::now() + Duration::from_secs(2);
    while !saw_cancel.load(Ordering::SeqCst) {
        assert!(
            std::time::Instant::now() < deadline,
            "dispatcher never observed pre-inserted cancel",
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    let result = outbound.recv_timeout(Duration::from_millis(200));
    assert!(
        result.is_err(),
        "expected no envelope after pre-cancel; got {result:?}",
    );
}

// Compile-time check: `Dispatch` is object-safe.
const _: fn(Arc<dyn Dispatch>) = |_| {};
