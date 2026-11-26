//! Sends from a thread that delivers work never wait on a full outbound
//! channel; they queue behind it in order. Other threads still wait.

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
fn an_inline_host_on_the_delivering_thread_never_waits_for_room() {
    let init = Runtime::mock();
    init.runtime.register_host(Chatty::default());
    fill(&init);
    let runtime = Arc::clone(&init.runtime);
    let reply = on_thread(move || {
        runtime.serve_peer_call(CallId(1), "test.chatty", None, "go".into(), Vec::new())
    });
    // Everything it sent waited behind the full channel, in order: the
    // notification, the call, the cancel of the call it dropped.
    let order = drained_after_fill(&init, 3);
    assert_eq!(order, ["first", "second", "cancel"]);
    // The answer itself may come in place or as a frame behind them.
    assert!(matches!(reply, Some(Ok(_)) | None));
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
