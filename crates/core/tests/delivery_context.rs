//! Sends from a delivery thread (inside a `DeliveryScope`) never wait on
//! a full outbound channel; they queue behind it in order. Other
//! threads, inline hosts polled on them included, still wait.

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, DEFAULT_OUTBOUND_CAPACITY, DeliveryScope, DirectOutcome, DirectPeer,
    Dispatch, DispatchFuture, DispatchMode, Frame, InstanceId, Outcome, Runtime, RuntimeInit,
};

/// Fill the outbound channel up to its capacity with `Notify("fill")`.
fn fill(init: &RuntimeInit) {
    for _ in 0..DEFAULT_OUTBOUND_CAPACITY {
        init.runtime
            .notify("peer.p", None, "fill", Vec::new())
            .expect("room");
    }
}

/// The methods of every frame queued after the filler, in order.
fn drained_after_fill(init: &RuntimeInit, expect: usize) -> Vec<String> {
    let mut seen = Vec::new();
    while seen.len() < expect {
        let envelope = init
            .outbound
            .recv_timeout(Duration::from_secs(5))
            .expect("frame");
        match envelope.frame {
            Frame::Notify { method, .. } if method == "fill" => {}
            Frame::Notify { method, .. } | Frame::Call { method, .. } => seen.push(method),
            Frame::Cancel { .. } => seen.push("cancel".to_owned()),
            other => panic!("unexpected {other:?}"),
        }
    }
    seen
}

/// An inline host that calls the peer twice while it runs.
#[derive(Default)]
struct Chatty {
    runtime: OnceLock<Weak<Runtime>>,
}

impl Dispatch for Chatty {
    fn plugin_id(&self) -> &'static str {
        "test.chatty"
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = self.runtime.set(runtime);
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let runtime = self.runtime.get().and_then(Weak::upgrade).expect("runtime");
        Box::pin(async move {
            runtime
                .notify("peer.p", None, "first", Vec::new())
                .expect("notify");
            let call = runtime
                .call("peer.p", None, "second", Vec::new())
                .expect("call");
            drop(call);
            Ok(Outcome::Ok(Vec::new()))
        })
    }
}

fn on_thread<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = flume::bounded(1);
    std::thread::spawn(move || {
        let _ = tx.send(work());
    });
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the delivering thread blocked on the full outbound channel")
}

#[test]
fn an_inline_host_on_a_delivery_thread_never_waits_for_room() {
    let init = Runtime::mock();
    init.runtime.register_host(Chatty::default());
    fill(&init);
    let runtime = Arc::clone(&init.runtime);
    let reply = on_thread(move || {
        let _pump = DeliveryScope::enter();
        runtime.serve_peer_call(CallId(1), "test.chatty", None, "go".into(), Vec::new())
    });
    // Everything it sent waited behind the full channel, in order: the
    // notification, the call, the cancel of the call it dropped.
    let order = drained_after_fill(&init, 3);
    assert_eq!(order, ["first", "second", "cancel"]);
    // The answer itself may come in place or as a frame behind them.
    assert!(matches!(reply, Some(Ok(_)) | None));
}

/// An inline host that notifies the peer once per call.
#[derive(Default)]
struct Notifier {
    runtime: OnceLock<Weak<Runtime>>,
}

impl Dispatch for Notifier {
    fn plugin_id(&self) -> &'static str {
        "test.notifier"
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = self.runtime.set(runtime);
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let runtime = self.runtime.get().and_then(Weak::upgrade).expect("runtime");
        let payload = payload.to_vec();
        Box::pin(async move {
            runtime
                .notify("peer.p", None, "tick", payload)
                .expect("notify");
            Ok(Outcome::Ok(Vec::new()))
        })
    }
}

#[test]
fn a_plain_thread_calling_inline_hosts_keeps_the_backpressure() {
    let init = Runtime::mock();
    init.runtime.register_host(Notifier::default());
    let calls = 3 * DEFAULT_OUTBOUND_CAPACITY;
    let runtime = Arc::clone(&init.runtime);
    // A Kotlin or app thread calling an inline host in a loop, while the
    // transport does not drain.
    let caller = std::thread::spawn(move || {
        for n in 0..calls {
            let id = u64::try_from(n).expect("id") + 1;
            let bytes = u32::try_from(n).expect("n").to_le_bytes().to_vec();
            runtime.serve_peer_call(CallId(id), "test.notifier", None, "go".into(), bytes);
        }
    });
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !caller.is_finished(),
        "the caller waits for room instead of queueing without bound"
    );
    assert!(init.outbound.is_full());
    // Draining lets it through, every tick in order.
    let mut next = 0u32;
    while next < u32::try_from(calls).expect("calls") {
        let envelope = init
            .outbound
            .recv_timeout(Duration::from_secs(5))
            .expect("frame");
        if let Frame::Notify { payload, .. } = envelope.frame {
            assert_eq!(payload, next.to_le_bytes());
            next += 1;
        }
    }
    caller.join().expect("caller");
}

#[test]
fn a_transport_thread_in_a_delivery_scope_never_waits_for_room() {
    let init = Runtime::mock();
    fill(&init);
    let runtime = Arc::clone(&init.runtime);
    on_thread(move || {
        let _scope = DeliveryScope::enter();
        for method in ["a", "b", "c"] {
            runtime
                .notify("peer.p", None, method, Vec::new())
                .expect("queued");
        }
    });
    // A thread outside any scope still waits its turn, behind them.
    let runtime = Arc::clone(&init.runtime);
    let waiter = std::thread::spawn(move || {
        runtime
            .notify("peer.p", None, "d", Vec::new())
            .expect("sent");
    });
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !waiter.is_finished(),
        "a plain thread keeps the backpressure"
    );
    assert_eq!(drained_after_fill(&init, 4), ["a", "b", "c", "d"]);
    waiter.join().expect("waiter");
}

#[test]
fn the_backlog_returns_to_zero_once_queued_frames_drain() {
    let init = Runtime::mock();
    fill(&init);
    let runtime = Arc::clone(&init.runtime);
    on_thread(move || {
        let _scope = DeliveryScope::enter();
        runtime
            .notify("peer.p", None, "queued", Vec::new())
            .expect("queued");
    });
    let mut frames = 0;
    while let Ok(envelope) = init.outbound.recv_timeout(Duration::from_millis(200)) {
        assert!(matches!(envelope.frame, Frame::Notify { .. }));
        init.runtime.outbound_delivered();
        frames += 1;
    }
    assert_eq!(frames, DEFAULT_OUTBOUND_CAPACITY + 1);
    // Every frame was reported delivered: calls go to the peer directly
    // again.
    init.runtime.install_direct_peer(Arc::new(Answers));
    let reply = init
        .runtime
        .call("peer.p", None, "after", Vec::new())
        .expect("call");
    assert!(init.outbound.try_recv().is_err(), "not queued");
    assert_eq!(reply.recv_blocking().expect("recv"), Ok(vec![1]));
}

/// Answers every call in place.
struct Answers;

impl DirectPeer for Answers {
    fn call(
        &self,
        _call_id: CallId,
        _plugin_id: &str,
        _instance_id: Option<InstanceId>,
        _method: &str,
        _payload: &[u8],
    ) -> DirectOutcome {
        DirectOutcome::Answered(Ok(vec![1]))
    }
}

#[test]
fn shutdown_frees_a_sender_waiting_behind_queued_frames() {
    let init = Runtime::mock();
    fill(&init);
    let runtime = Arc::clone(&init.runtime);
    on_thread(move || {
        let _scope = DeliveryScope::enter();
        runtime
            .notify("peer.p", None, "queued", Vec::new())
            .expect("queued");
    });
    let runtime = Arc::clone(&init.runtime);
    let waiter = std::thread::spawn(move || runtime.notify("peer.p", None, "late", Vec::new()));
    std::thread::sleep(Duration::from_millis(50));
    init.runtime.shutdown();
    let result = waiter.join().expect("waiter");
    assert!(result.is_err(), "the waiting sender gave up at shutdown");
}

#[test]
fn the_runtime_counts_what_it_queued() {
    let init = Runtime::mock();
    fill(&init);
    let queued = 5 * DEFAULT_OUTBOUND_CAPACITY;
    let runtime = Arc::clone(&init.runtime);
    on_thread(move || {
        let _scope = DeliveryScope::enter();
        for _ in 0..queued {
            runtime
                .notify("peer.p", None, "queued", Vec::new())
                .expect("queued");
        }
    });
    let stats = init.runtime.outbound_spill();
    // The drain may have moved a few already.
    assert!(
        stats.queued + DEFAULT_OUTBOUND_CAPACITY >= queued,
        "{stats:?}"
    );
    assert!(stats.peak >= stats.queued, "{stats:?}");
    assert_eq!(
        stats.total,
        u64::try_from(queued).expect("total"),
        "{stats:?}"
    );
    let mut frames = 0;
    while frames < DEFAULT_OUTBOUND_CAPACITY + queued {
        init.outbound
            .recv_timeout(Duration::from_secs(5))
            .expect("frame");
        frames += 1;
    }
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while init.runtime.outbound_spill().queued > 0 && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(init.runtime.outbound_spill().queued, 0);
}

#[test]
fn a_cancel_is_never_dropped_on_a_full_channel() {
    let init = Runtime::mock();
    for _ in 1..DEFAULT_OUTBOUND_CAPACITY {
        init.runtime
            .notify("peer.p", None, "fill", Vec::new())
            .expect("room");
    }
    // The call takes the last slot; dropping it sends a cancel that
    // must not wait and finds the channel full.
    let call = init
        .runtime
        .call("peer.p", None, "dropped", Vec::new())
        .expect("call");
    assert!(init.outbound.is_full());
    drop(call);
    assert_eq!(drained_after_fill(&init, 2), ["dropped", "cancel"]);
}
