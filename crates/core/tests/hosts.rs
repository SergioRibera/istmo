use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, Envelope, Frame, InstanceId, Outcome, Runtime,
    StreamEndReason, StreamMessage, codec,
};

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
        if let Ok(env) = outbound.recv_timeout(Duration::from_millis(50))
            && let Frame::Respond { call_id: cid, .. } = &env.frame
            && *cid == call_id
        {
            return env.frame;
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

    std::thread::sleep(Duration::from_millis(10));
    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .expect("dispatch cancel");

    let result = outbound.recv_timeout(Duration::from_millis(200));
    assert!(
        result.is_err(),
        "expected no envelope after cancel; got {result:?}",
    );
}

#[test]
fn runtime_notify_local_short_circuits_to_registered_host() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;
    let dispatcher = Arc::new(EchoDispatch::new());
    rt.register_host(ArcHostAdapter(dispatcher.clone()));

    rt.notify(EchoDispatch::PLUGIN_ID, None, "release", vec![1, 2, 3])
        .unwrap();

    for _ in 0..1_000 {
        if dispatcher.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);

    assert!(
        outbound.try_recv().is_err(),
        "notify to local host must not touch the outbound channel",
    );
}

#[test]
fn runtime_notify_without_local_host_emits_outbound_notify_frame() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;

    rt.notify("unregistered.plugin", None, "release", vec![9, 9, 9])
        .unwrap();

    let envelope = outbound
        .recv_timeout(Duration::from_secs(1))
        .expect("Notify frame");
    match envelope.frame {
        Frame::Notify {
            plugin_id,
            instance_id,
            method,
            payload,
        } => {
            assert_eq!(plugin_id, "unregistered.plugin");
            assert!(instance_id.is_none());
            assert_eq!(method, "release");
            assert_eq!(payload, vec![9, 9, 9]);
        }
        other => panic!("expected Notify, got {other:?}"),
    }
}

#[test]
fn inject_wire_envelope_bridges_wire_bytes_into_dispatch() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;
    rt.register_host(EchoDispatch::new());

    let call_id = CallId(1234);
    let envelope = Envelope::new(Frame::Call {
        call_id,
        plugin_id: EchoDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "cross_process".to_owned(),
        payload: codec::encode(&()).unwrap(),
    });
    let wire = envelope.to_wire_bytes().expect("encode");

    rt.inject_wire_envelope(&wire).expect("inject bytes");

    let respond = wait_for_respond(&outbound, call_id);
    match respond {
        Frame::Respond { result, .. } => {
            assert!(result.is_ok(), "cross-process echo should succeed");
        }
        other => panic!("expected Respond, got {other:?}"),
    }
}

#[test]
fn inject_wire_envelope_rejects_stale_protocol_version() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let stale = Envelope {
        version: istmo_core::PROTOCOL_VERSION - 1,
        frame: Frame::Cancel { call_id: CallId(1) },
    };
    let bytes = codec::encode(&stale).unwrap();

    let err = rt
        .inject_wire_envelope(&bytes)
        .expect_err("stale envelope should reject");
    assert!(matches!(
        err,
        istmo_core::IstmoError::ProtocolVersionMismatch { .. },
    ));
}

#[test]
fn inbound_notify_routes_to_registered_host_without_response() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;
    let dispatcher = Arc::new(EchoDispatch::new());
    rt.register_host(ArcHostAdapter(dispatcher.clone()));

    rt.dispatch_inbound(Envelope::new(Frame::Notify {
        plugin_id: EchoDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "release".to_owned(),
        payload: codec::encode(&()).unwrap(),
    }))
    .unwrap();

    for _ in 0..1_000 {
        if dispatcher.calls.load(Ordering::SeqCst) > 0 {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
    assert!(
        outbound.try_recv().is_err(),
        "inbound Notify must never emit a Respond",
    );
}

struct ArcHostAdapter(Arc<EchoDispatch>);

impl Dispatch for ArcHostAdapter {
    fn plugin_id(&self) -> &'static str {
        EchoDispatch::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        self.0.dispatch(instance_id, method, payload, cancel)
    }
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

    let result = outbound.recv_timeout(Duration::from_millis(200));
    assert!(
        result.is_err(),
        "expected no envelope after cancel; got {result:?}",
    );
}

#[test]
fn cancel_arriving_before_dispatch_starts_trips_token_immediately() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let saw_cancel = Arc::new(AtomicBool::new(false));
    rt.register_host(CooperativeDispatch {
        saw_cancel: Arc::clone(&saw_cancel),
    });

    let call_id = CallId(505);

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

const _: fn(Arc<dyn Dispatch>) = |_| {};

/// Rust-hosted stateful plugin: `create_instance` decodes a `u32`
/// seed, `dispatch("get")` returns the seed for its instance,
/// `destroy_instance` removes it. Verifies the framework wiring for
/// stateful dispatchers hosted in the same process.
struct SeedCounter {
    next_id: AtomicU32,
    instances: std::sync::Mutex<std::collections::HashMap<InstanceId, u32>>,
    destroyed: std::sync::Mutex<Vec<InstanceId>>,
}

impl SeedCounter {
    const PLUGIN_ID: &'static str = "test.seed_counter";

    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            instances: std::sync::Mutex::new(std::collections::HashMap::new()),
            destroyed: std::sync::Mutex::new(Vec::new()),
        }
    }
}

impl Dispatch for SeedCounter {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn create_instance<'a>(
        &'a self,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            let (seed, _): (u32, _) = codec::decode(payload).unwrap();
            let id = InstanceId::new(u64::from(self.next_id.fetch_add(1, Ordering::SeqCst)));
            self.instances.lock().unwrap().insert(id, seed);
            Ok(Outcome::Ok(codec::encode(&id).unwrap()))
        })
    }

    fn destroy_instance(&self, instance_id: InstanceId) {
        self.instances.lock().unwrap().remove(&instance_id);
        self.destroyed.lock().unwrap().push(instance_id);
    }

    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let id = instance_id.expect("stateful dispatch requires an instance id");
        let seed = *self
            .instances
            .lock()
            .unwrap()
            .get(&id)
            .expect("known instance");
        let method_owned = method.to_owned();
        Box::pin(async move {
            match method_owned.as_str() {
                "get" => Ok(Outcome::Ok(codec::encode(&seed).unwrap())),
                other => Err(istmo_core::DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

#[test]
fn hosted_stateful_plugin_round_trips_create_call_destroy() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let dispatcher = Arc::new(SeedCounter::new());
    rt.register_host(SeedHost(dispatcher.clone()));

    // create_instance short-circuits to hosted dispatcher.
    let seed_a = 42u32;
    let handle = rt
        .create_instance(SeedCounter::PLUGIN_ID, codec::encode(&seed_a).unwrap())
        .expect("create_instance");
    let bytes = pollster::block_on(handle)
        .expect("create ok")
        .expect("create Ok");
    let (instance_a, _): (InstanceId, _) = codec::decode(&bytes).unwrap();

    // call round-trip returns the seed stored under this instance id.
    let handle = rt
        .call(SeedCounter::PLUGIN_ID, Some(instance_a), "get", Vec::new())
        .expect("call");
    let bytes = pollster::block_on(handle).expect("call ok").expect("Ok");
    let (returned, _): (u32, _) = codec::decode(&bytes).unwrap();
    assert_eq!(returned, seed_a);

    // A second instance is independent.
    let seed_b = 100u32;
    let handle = rt
        .create_instance(SeedCounter::PLUGIN_ID, codec::encode(&seed_b).unwrap())
        .expect("second create");
    let bytes = pollster::block_on(handle).expect("create ok").expect("Ok");
    let (instance_b, _): (InstanceId, _) = codec::decode(&bytes).unwrap();
    assert_ne!(instance_a, instance_b);

    // destroy_instance triggers the host's callback.
    rt.destroy_instance(instance_a).expect("destroy");

    // Give the destroy a beat — dispatch_inbound routes it synchronously
    // but pollster block on a spawned thread means Destroy is inline.
    let destroyed = dispatcher.destroyed.lock().unwrap().clone();
    assert!(destroyed.contains(&instance_a));

    // Outbound channel stays empty for the hosted round-trips.
    assert!(outbound.try_recv().is_err());
}

/// Adapter used above so a shared `Arc<SeedCounter>` can be registered
/// on the runtime AND kept for direct assertions on internal state.
struct SeedHost(Arc<SeedCounter>);

impl Dispatch for SeedHost {
    fn plugin_id(&self) -> &'static str {
        self.0.plugin_id()
    }

    fn create_instance<'a>(&'a self, payload: &'a [u8], cancel: CancelToken) -> DispatchFuture<'a> {
        self.0.create_instance(payload, cancel)
    }

    fn destroy_instance(&self, instance_id: InstanceId) {
        self.0.destroy_instance(instance_id);
    }

    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        self.0.dispatch(instance_id, method, payload, cancel)
    }
}

struct ParkUntilCancelled {
    observed: flume::Sender<()>,
}

impl Dispatch for ParkUntilCancelled {
    fn plugin_id(&self) -> &'static str {
        "test.park"
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            cancel.cancelled().await;
            self.observed.send(()).expect("test still listening");
            Ok(Outcome::Ok(Vec::new()))
        })
    }
}

#[test]
fn dropping_a_locally_hosted_call_trips_its_cancel_token() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;
    let (observed, cancelled) = flume::bounded(1);
    rt.register_host(ParkUntilCancelled { observed });

    let handle = rt
        .call("test.park", None, "park", Vec::new())
        .expect("call dispatched");
    drop(handle);

    cancelled
        .recv_timeout(Duration::from_secs(1))
        .expect("hosted dispatch observed the cancellation");
    assert!(
        outbound.try_recv().is_err(),
        "a locally hosted call must not leak a Cancel frame to the peer"
    );
}

/// Stream host: `three` yields three items and closes, `forever` yields
/// until the stream is cancelled and then reports on `stopped`, `panic`
/// panics inside the dispatch.
struct StreamHost {
    stopped: flume::Sender<()>,
}

impl StreamHost {
    const PLUGIN_ID: &'static str = "test.stream";
}

impl Dispatch for StreamHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        method: &'a str,
        _payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let stopped = self.stopped.clone();
        Box::pin(async move {
            let (tx, rx) = flume::bounded(1);
            match method {
                "three" => {
                    std::thread::spawn(move || {
                        for i in 0u8..3 {
                            if tx.send(vec![i]).is_err() {
                                return;
                            }
                        }
                    });
                }
                "forever" => {
                    std::thread::spawn(move || {
                        while !cancel.is_cancelled() && tx.send(vec![0]).is_ok() {
                            std::thread::sleep(Duration::from_millis(1));
                        }
                        let _ = stopped.send(());
                    });
                }
                _ => panic!("stream host asked to panic"),
            }
            Ok(Outcome::StreamOpened(rx))
        })
    }
}

fn stream_host(rt: &Arc<Runtime>) -> flume::Receiver<()> {
    let (stopped, on_stop) = flume::unbounded();
    rt.register_host(StreamHost { stopped });
    on_stop
}

#[test]
fn hosted_stream_delivers_items_then_ends_without_outbound_frames() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let _on_stop = stream_host(&rt);

    let handle = rt
        .stream(StreamHost::PLUGIN_ID, None, "three", Vec::new(), 0)
        .expect("stream");
    for i in 0u8..3 {
        assert_eq!(handle.recv().expect("item"), StreamMessage::Event(vec![i]));
    }
    assert_eq!(
        handle.recv().expect("end"),
        StreamMessage::End(StreamEndReason::Complete)
    );
    drop(handle);
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        init.outbound.try_recv().is_err(),
        "a locally hosted stream must not reach the transport"
    );
}

#[test]
fn dropping_a_hosted_stream_stops_its_producer() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let on_stop = stream_host(&rt);

    let handle = rt
        .stream(StreamHost::PLUGIN_ID, None, "forever", Vec::new(), 1)
        .expect("stream");
    assert!(matches!(
        handle.recv().expect("item"),
        StreamMessage::Event(_)
    ));
    drop(handle);

    on_stop
        .recv_timeout(Duration::from_secs(1))
        .expect("producer stopped after the consumer left");
    std::thread::sleep(Duration::from_millis(20));
    assert!(
        init.outbound.try_recv().is_err(),
        "dropping a hosted stream must not send Cancel, Event or StreamEnd"
    );
}

#[test]
fn inbound_cancel_ends_a_peer_hosted_stream() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let on_stop = stream_host(&rt);
    let call_id = CallId(77);

    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: StreamHost::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "forever".to_owned(),
        payload: Vec::new(),
    }))
    .expect("dispatch");
    let first = init
        .outbound
        .recv_timeout(Duration::from_secs(1))
        .expect("first event");
    assert!(matches!(first.frame, Frame::Event { .. }));

    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .expect("cancel");
    on_stop
        .recv_timeout(Duration::from_secs(1))
        .expect("producer stopped after the peer cancelled");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    loop {
        let env = init
            .outbound
            .recv_deadline(deadline)
            .expect("stream end before the deadline");
        if let Frame::StreamEnd { stream_id, reason } = env.frame {
            assert_eq!(stream_id.get(), call_id.get());
            assert_eq!(reason, StreamEndReason::Cancelled);
            break;
        }
    }
}

#[test]
fn panicking_stream_dispatch_ends_the_local_stream_with_an_error() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let _on_stop = stream_host(&rt);

    let handle = rt
        .stream(StreamHost::PLUGIN_ID, None, "panic", Vec::new(), 0)
        .expect("stream");
    let deadline = std::time::Instant::now() + Duration::from_secs(1);
    let message = loop {
        if let Some(message) = handle.try_recv() {
            break message;
        }
        assert!(std::time::Instant::now() < deadline, "stream never ended");
        std::thread::sleep(Duration::from_millis(2));
    };
    match message {
        StreamMessage::End(StreamEndReason::Error(bytes)) => {
            let (msg, _) = codec::decode::<String>(&bytes).expect("decode");
            assert_eq!(msg, "host panicked");
        }
        other => panic!("expected an error end, got {other:?}"),
    }
}

struct PanicHost;

impl Dispatch for PanicHost {
    fn plugin_id(&self) -> &'static str {
        "test.panic"
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async { panic!("host asked to panic") })
    }
}

#[test]
fn panicking_host_answers_a_local_call_with_an_error() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.register_host(PanicHost);

    let handle = rt
        .call("test.panic", None, "any", Vec::new())
        .expect("call");
    let bytes = pollster::block_on(handle)
        .expect("resolves")
        .expect_err("error result");
    let (msg, _) = codec::decode::<String>(&bytes).expect("decode");
    assert_eq!(msg, "host panicked");
}

#[test]
fn panicking_host_answers_a_peer_call_with_an_error() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.register_host(PanicHost);
    let call_id = CallId(31);

    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "test.panic".to_owned(),
        instance_id: None,
        method: "any".to_owned(),
        payload: Vec::new(),
    }))
    .expect("dispatch");
    match wait_for_respond(&init.outbound, call_id) {
        Frame::Respond {
            result: Err(bytes), ..
        } => {
            let (msg, _) = codec::decode::<String>(&bytes).expect("decode");
            assert_eq!(msg, "host panicked");
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}

#[test]
fn peer_call_ids_do_not_collide_with_local_pending_calls() {
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.register_host(EchoDispatch::new());

    // A local call to a peer-hosted plugin takes id 1.
    let local = rt.call("peer.plugin", None, "m", Vec::new()).expect("call");
    assert_eq!(local.call_id(), CallId(1));
    let _ = init.outbound.recv().expect("call frame");

    // The peer independently numbers its own call 1 too.
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id: CallId(1),
        plugin_id: EchoDispatch::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "ping".to_owned(),
        payload: b"x".to_vec(),
    }))
    .expect("dispatch");
    assert!(matches!(
        wait_for_respond(&init.outbound, CallId(1)),
        Frame::Respond { result: Ok(_), .. }
    ));
    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id: CallId(1),
        result: Ok(vec![5]),
    }))
    .expect("the peer's response must not have completed the local call");
    assert_eq!(pollster::block_on(local).expect("resolves"), Ok(vec![5]));
}
