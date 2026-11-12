//! Races between hosted calls, cancels, full channels and shutdown.
//!
//! Every wait is bounded, so a regression shows up as a failure rather
//! than a hung test run.

use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, Envelope, Frame, InstanceId, IstmoError,
    Outcome, Runtime, StreamEndReason, StreamId, StreamMessage,
};

const DEADLINE: Duration = Duration::from_secs(5);

/// Answers `echo` with its payload; `gate` waits for the test to open
/// the gate; `flood` opens a stream whose producer only stops once its
/// items stop being taken.
struct Host {
    gate: flume::Receiver<()>,
    producer_stopped: flume::Sender<()>,
}

impl Host {
    const PLUGIN_ID: &'static str = "test.concurrency";
}

impl Dispatch for Host {
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
        let gate = self.gate.clone();
        let stopped = self.producer_stopped.clone();
        Box::pin(async move {
            match method {
                "echo" => Ok(Outcome::Ok(payload.to_vec())),
                "gate" => {
                    let _ = gate.recv_timeout(DEADLINE);
                    Ok(Outcome::Ok(payload.to_vec()))
                }
                "flood" => {
                    let (tx, rx) = flume::bounded(1);
                    thread::spawn(move || {
                        while tx.send(Ok(vec![0])).is_ok() {}
                        let _ = stopped.send(());
                    });
                    Ok(Outcome::StreamOpened(rx))
                }
                other => Err(istmo_core::DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

struct Fixture {
    rt: Arc<Runtime>,
    outbound: flume::Receiver<Envelope>,
    open_gate: flume::Sender<()>,
    producer_stopped: flume::Receiver<()>,
}

fn fixture() -> Fixture {
    let init = Runtime::mock();
    let (open_gate, gate) = flume::unbounded();
    let (stopped_tx, producer_stopped) = flume::unbounded();
    init.runtime.register_host(Host {
        gate,
        producer_stopped: stopped_tx,
    });
    Fixture {
        rt: init.runtime,
        outbound: init.outbound,
        open_gate,
        producer_stopped,
    }
}

fn call_frame(call_id: u64, method: &str) -> Envelope {
    Envelope::new(Frame::Call {
        call_id: CallId(call_id),
        plugin_id: Host::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: method.to_owned(),
        payload: call_id.to_le_bytes().to_vec(),
    })
}

/// Wait until every host thread let go of the runtime.
fn wait_for_sole_owner(rt: &Arc<Runtime>) {
    let deadline = Instant::now() + DEADLINE;
    while Arc::strong_count(rt) > 1 {
        assert!(
            Instant::now() < deadline,
            "host threads still hold the runtime: {}",
            Arc::strong_count(rt)
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// Fill the outbound channel with frames nobody drains.
fn fill_outbound(rt: &Arc<Runtime>) {
    for _ in 0..istmo_core::DEFAULT_OUTBOUND_CAPACITY {
        rt.notify("test.nobody", None, "fill", Vec::new())
            .expect("room for filler");
    }
}

#[test]
fn peer_and_remote_calls_with_the_same_id_answer_their_own_issuer() {
    let fx = fixture();
    let sink_log = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&sink_log);
    fx.rt
        .install_remote_envelope_sink(Arc::new(move |bytes| sink.lock().unwrap().push(bytes)));

    fx.rt
        .dispatch_inbound(call_frame(1, "gate"))
        .expect("peer call");
    let remote = call_frame(1, "gate").to_wire_bytes().expect("encode");
    fx.rt.inject_wire_envelope(&remote).expect("remote call");

    // The remote side cancels its call 1; the peer's call 1 must live on.
    let cancel = Envelope::new(Frame::Cancel { call_id: CallId(1) })
        .to_wire_bytes()
        .expect("encode");
    fx.rt.inject_wire_envelope(&cancel).expect("remote cancel");
    fx.open_gate.send(()).unwrap();
    fx.open_gate.send(()).unwrap();

    let env = fx.outbound.recv_timeout(DEADLINE).expect("peer response");
    assert!(matches!(
        env.frame,
        Frame::Respond {
            call_id: CallId(1),
            result: Ok(_)
        }
    ));
    wait_for_sole_owner(&fx.rt);
    assert!(
        sink_log.lock().unwrap().is_empty(),
        "the cancelled remote call must not be answered"
    );
    assert!(fx.outbound.try_recv().is_err());
}

#[test]
fn remote_calls_answer_through_the_sink() {
    let fx = fixture();
    let (sink_tx, sink_rx) = flume::unbounded();
    fx.rt
        .install_remote_envelope_sink(Arc::new(move |bytes| drop(sink_tx.send(bytes))));
    let call = call_frame(7, "echo").to_wire_bytes().expect("encode");
    fx.rt.inject_wire_envelope(&call).expect("remote call");

    let bytes = sink_rx.recv_timeout(DEADLINE).expect("response via sink");
    let env = Envelope::from_wire_bytes(&bytes).expect("decode");
    assert!(matches!(
        env.frame,
        Frame::Respond { call_id: CallId(7), result: Ok(ref p) } if *p == 7u64.to_le_bytes()
    ));
    assert!(fx.outbound.try_recv().is_err());
}

#[test]
fn shutdown_frees_a_local_stream_pump_blocked_on_a_full_consumer() {
    let fx = fixture();
    let handle = fx
        .rt
        .stream(Host::PLUGIN_ID, None, "flood", Vec::new(), 1)
        .expect("stream");
    thread::sleep(Duration::from_millis(20));

    fx.rt.shutdown();
    fx.producer_stopped
        .recv_timeout(DEADLINE)
        .expect("producer released while the consumer still holds its handle");
    wait_for_sole_owner(&fx.rt);
    drop(handle);
}

#[test]
fn shutdown_frees_a_peer_stream_pump_blocked_on_a_full_outbound_channel() {
    let fx = fixture();
    fx.rt
        .dispatch_inbound(call_frame(3, "flood"))
        .expect("peer stream");
    let deadline = Instant::now() + DEADLINE;
    while !fx.outbound.is_full() {
        assert!(Instant::now() < deadline, "outbound never filled");
        thread::sleep(Duration::from_millis(5));
    }

    fx.rt.shutdown();
    fx.producer_stopped
        .recv_timeout(DEADLINE)
        .expect("producer released although nobody drains");
    wait_for_sole_owner(&fx.rt);
}

#[test]
fn a_peer_call_finishing_after_shutdown_does_not_wait_on_a_full_outbound_channel() {
    let fx = fixture();
    fx.rt
        .dispatch_inbound(call_frame(4, "gate"))
        .expect("peer call");
    fill_outbound(&fx.rt);

    fx.rt.shutdown();
    fx.open_gate.send(()).unwrap();
    wait_for_sole_owner(&fx.rt);
}

#[test]
fn a_peer_cancel_frees_a_response_waiting_on_a_full_outbound_channel() {
    let fx = fixture();
    fx.rt
        .dispatch_inbound(call_frame(5, "gate"))
        .expect("peer call");
    fill_outbound(&fx.rt);
    fx.open_gate.send(()).unwrap();
    thread::sleep(Duration::from_millis(20));

    fx.rt
        .dispatch_inbound(Envelope::new(Frame::Cancel { call_id: CallId(5) }))
        .expect("cancel");
    wait_for_sole_owner(&fx.rt);
}

#[test]
fn an_error_response_to_a_stream_call_ends_the_stream() {
    let init = Runtime::mock();
    let handle = init
        .runtime
        .stream("test.elsewhere", None, "s", Vec::new(), 0)
        .expect("stream");
    init.runtime
        .dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: CallId(handle.stream_id().get()),
            result: Err(vec![9]),
        }))
        .expect("respond");
    assert_eq!(
        handle.recv().expect("end"),
        StreamMessage::End(StreamEndReason::Error(vec![9]))
    );
}

#[test]
fn concurrent_calls_and_drops_all_settle() {
    let fx = fixture();
    let workers: Vec<_> = (0..16)
        .map(|worker| {
            let rt = Arc::clone(&fx.rt);
            thread::spawn(move || {
                for i in 0..200u32 {
                    let handle = rt
                        .call(Host::PLUGIN_ID, None, "echo", i.to_le_bytes().to_vec())
                        .expect("call");
                    if (i + worker) % 3 == 0 {
                        drop(handle);
                    } else {
                        let result = handle.recv_blocking().expect("answered");
                        assert_eq!(result, Ok(i.to_le_bytes().to_vec()));
                    }
                    let stream = rt
                        .stream(Host::PLUGIN_ID, None, "flood", Vec::new(), 1)
                        .expect("stream");
                    assert!(matches!(stream.recv(), Ok(StreamMessage::Event(_))));
                }
            })
        })
        .collect();
    for worker in workers {
        worker.join().expect("worker");
    }
    for _ in 0..16 * 200 {
        fx.producer_stopped
            .recv_timeout(DEADLINE)
            .expect("every dropped stream stops its producer");
    }
    wait_for_sole_owner(&fx.rt);
}

#[test]
fn calls_racing_shutdown_never_hang() {
    for _ in 0..20 {
        let fx = fixture();
        let (done_tx, done_rx) = flume::unbounded();
        let callers: Vec<_> = (0..8)
            .map(|_| {
                let rt = Arc::clone(&fx.rt);
                let done = done_tx.clone();
                thread::spawn(move || {
                    for _ in 0..50 {
                        let outcome = rt
                            .call(Host::PLUGIN_ID, None, "echo", Vec::new())
                            .and_then(istmo_core::CallHandle::recv_blocking);
                        match outcome {
                            Ok(_)
                            | Err(IstmoError::RuntimeShutDown | IstmoError::ChannelClosed) => {}
                            Err(err) => panic!("unexpected error: {err}"),
                        }
                        let outcome = rt
                            .stream("test.elsewhere", None, "s", Vec::new(), 0)
                            .map(|stream| stream.stream_id());
                        match outcome {
                            Ok(StreamId(_)) | Err(IstmoError::RuntimeShutDown) => {}
                            Err(err) => panic!("unexpected error: {err}"),
                        }
                    }
                    done.send(()).expect("collector alive");
                })
            })
            .collect();
        thread::sleep(Duration::from_millis(1));
        fx.rt.shutdown();
        drop(done_tx);
        for _ in 0..callers.len() {
            done_rx
                .recv_timeout(DEADLINE)
                .expect("a caller hung across shutdown");
        }
        for caller in callers {
            caller.join().expect("caller");
        }
    }
}
