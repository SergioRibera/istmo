//! End-to-end verification of `#[istmo::plugin]` and `#[istmo::message]`.
//!
//! Each test spins up a mock backend on a background thread that services
//! whatever frames the generated client emits, then exercises the client.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use istmo::{CallId, CancelToken, Envelope, Frame, Runtime, StreamEndReason, StreamItem, codec};

#[istmo::message]
#[derive(Debug, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

impl core::fmt::Display for EchoError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.reason)
    }
}

impl std::error::Error for EchoError {}

#[istmo::plugin(name = "com.example.echo")]
pub trait Echo {
    async fn ping(&self, msg: String) -> Result<String, EchoError>;

    async fn add(&self, a: i32, b: i32) -> i32;

    #[istmo::stream]
    fn ticks(&self, count: u32) -> u32;
}

#[test]
fn generated_client_round_trips_unary_calls() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        for _ in 0..2 {
            let envelope = outbound.recv().expect("call frame");
            let Frame::Call {
                call_id,
                method,
                payload,
                ..
            } = envelope.frame
            else {
                panic!("expected call");
            };
            let result = match method.as_str() {
                "ping" => {
                    let ((msg,), _) = codec::decode::<(String,)>(&payload).unwrap();
                    Ok(codec::encode(&format!("pong:{msg}")).unwrap())
                }
                "add" => {
                    let ((a, b), _) = codec::decode::<(i32, i32)>(&payload).unwrap();
                    Ok(codec::encode(&(a + b)).unwrap())
                }
                other => panic!("unexpected method {other}"),
            };
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))
                .unwrap();
        }
    });

    let echo = EchoClient::from_runtime(&rt).expect("declared");
    assert_eq!(
        pollster::block_on(echo.ping("hi".to_owned())).unwrap(),
        "pong:hi",
    );
    assert_eq!(pollster::block_on(echo.add(2, 3)).unwrap(), 5);
    backend.join().unwrap();
}

#[test]
fn generated_client_surfaces_domain_errors_as_plugin_error_bytes() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let envelope = outbound.recv().expect("call");
        let Frame::Call { call_id, .. } = envelope.frame else {
            panic!("expected call");
        };
        let error_payload = codec::encode(&EchoError {
            reason: "nope".to_owned(),
        })
        .unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Err(error_payload),
            }))
            .unwrap();
    });

    let echo = EchoClient::from_runtime(&rt).expect("declared");
    let err = pollster::block_on(echo.ping("nope".to_owned())).expect_err("should fail");
    let bytes = match err {
        istmo::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<EchoError>(&bytes).unwrap();
    assert_eq!(
        decoded,
        EchoError {
            reason: "nope".to_owned()
        }
    );
    backend.join().unwrap();
}

// ---- Cancellation-aware trait DSL ---------------------------------------

/// Trait exercising the DSL: `cancel: CancelToken` is stripped from the
/// wire signature (client method + payload tuple) and gets filled by the
/// runtime on the host side.
#[istmo::plugin(name = "com.example.slow")]
pub trait Slow {
    async fn wait(&self, delay_ms: u32, cancel: CancelToken) -> u32;
}

/// Host impl that parks on the cancel token instead of the delay when the
/// runtime trips it. Records both branches so the test can assert.
#[derive(Debug, Default)]
struct SlowImpl {
    cancelled: Arc<AtomicBool>,
    started: Arc<AtomicBool>,
}

impl Slow for SlowImpl {
    async fn wait(&self, _delay_ms: u32, cancel: CancelToken) -> u32 {
        self.started.store(true, Ordering::SeqCst);
        cancel.cancelled().await;
        self.cancelled.store(true, Ordering::SeqCst);
        // The host respond frame is dropped by the runtime once the cancel
        // flag is set — no test asserts on the return value.
        0
    }
}

#[test]
fn cancel_token_arg_is_stripped_from_wire_and_client_signature() {
    // Wire payload for `wait(delay_ms)` must decode as `(u32,)` — proves
    // the CancelToken arg is not part of the encoded tuple.
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let envelope = outbound.recv().expect("call frame");
        let Frame::Call {
            call_id,
            method,
            payload,
            ..
        } = envelope.frame
        else {
            panic!("expected call");
        };
        assert_eq!(method, "wait");
        // If the cancel token had leaked into the payload, this decode
        // would either fail or leave trailing bytes.
        let ((delay_ms,), consumed) = codec::decode::<(u32,)>(&payload).unwrap();
        assert_eq!(delay_ms, 42);
        assert_eq!(
            consumed,
            payload.len(),
            "payload has no trailing cancel arg"
        );
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Ok(codec::encode(&99_u32).unwrap()),
            }))
            .unwrap();
    });

    let slow = SlowClient::from_runtime(&rt).expect("declared");
    // Client's `wait` takes only `delay_ms` — no CancelToken parameter.
    assert_eq!(pollster::block_on(slow.wait(42)).unwrap(), 99);
    backend.join().unwrap();
}

#[test]
fn host_dispatcher_forwards_runtime_cancel_to_trait_impl() {
    let init = Runtime::mock();
    let rt = init.runtime;

    let inner = SlowImpl::default();
    let started = inner.started.clone();
    let cancelled = inner.cancelled.clone();
    rt.register_host(SlowHost::new(inner));

    let call_id = CallId(9001);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "com.example.slow".to_owned(),
        instance_id: None,
        method: "wait".to_owned(),
        payload: codec::encode(&(100_u32,)).unwrap(),
    }))
    .unwrap();

    // Spin until the host thread has entered the impl body.
    for _ in 0..1_000 {
        if started.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(started.load(Ordering::SeqCst), "impl never entered");

    rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id }))
        .unwrap();

    for _ in 0..1_000 {
        if cancelled.load(Ordering::SeqCst) {
            break;
        }
        thread::sleep(std::time::Duration::from_millis(1));
    }
    assert!(
        cancelled.load(Ordering::SeqCst),
        "cancel token forwarded to impl never tripped",
    );
}

// ---- Hosted streams -----------------------------------------------------

#[istmo::plugin(name = "com.example.ticker")]
pub trait Ticker {
    #[istmo::stream]
    fn count_up(&self, to: u32) -> u32;
}

#[derive(Default)]
struct TickerImpl;

impl Ticker for TickerImpl {
    fn count_up(&self, to: u32) -> flume::Receiver<u32> {
        let (tx, rx) = flume::unbounded();
        std::thread::spawn(move || {
            for i in 0..to {
                if tx.send(i).is_err() {
                    return;
                }
            }
        });
        rx
    }
}

#[test]
fn hosted_stream_pumps_events_and_closes_with_stream_end() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;

    rt.register_host(TickerHost::new(TickerImpl));

    let call_id = CallId(4242);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "com.example.ticker".to_owned(),
        instance_id: None,
        method: "count_up".to_owned(),
        payload: codec::encode(&(3_u32,)).unwrap(),
    }))
    .unwrap();

    let mut events = Vec::new();
    let mut ended = false;
    while !ended {
        let envelope = outbound
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("stream frames");
        match envelope.frame {
            Frame::Event { stream_id, payload } => {
                assert_eq!(stream_id.get(), call_id.get());
                let (item, _) = codec::decode::<u32>(&payload).unwrap();
                events.push(item);
            }
            Frame::StreamEnd { stream_id, reason } => {
                assert_eq!(stream_id.get(), call_id.get());
                assert_eq!(reason, StreamEndReason::Complete);
                ended = true;
            }
            other => panic!("unexpected frame {other:?}"),
        }
    }
    assert_eq!(events, vec![0, 1, 2]);
}

#[test]
fn generated_client_reads_stream_events() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let envelope = outbound.recv().expect("call");
        let Frame::Call {
            call_id, payload, ..
        } = envelope.frame
        else {
            panic!("expected call");
        };
        let ((count,), _) = codec::decode::<(u32,)>(&payload).unwrap();
        let stream_id = istmo::StreamId(call_id.get());
        for tick in 0..count {
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Event {
                    stream_id,
                    payload: codec::encode(&tick).unwrap(),
                }))
                .unwrap();
        }
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::StreamEnd {
                stream_id,
                reason: StreamEndReason::Complete,
            }))
            .unwrap();
    });

    let echo = EchoClient::from_runtime(&rt).expect("declared");
    let stream = echo.ticks(3).expect("open");
    let mut collected = Vec::new();
    loop {
        match stream.recv().unwrap() {
            StreamItem::Event(v) => collected.push(v),
            StreamItem::Completed => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(collected, vec![0_u32, 1, 2]);
    backend.join().unwrap();
}
