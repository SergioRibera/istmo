//! Tests for [`DispatchMode::Inline`].

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::task::{Context, Poll};
use std::thread;
use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, DispatchMode, Envelope, Frame, InstanceId,
    Outcome, Runtime, StreamMessage,
};

/// Inline host that records the thread id of each poll into a shared
/// counter the test can read back.
struct InlineEcho {
    plugin_id: &'static str,
    thread_tag: Arc<AtomicU64>,
}

impl Dispatch for InlineEcho {
    fn plugin_id(&self) -> &'static str {
        self.plugin_id
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        self.thread_tag.store(thread_id_as_u64(), Ordering::SeqCst);
        let bytes = payload.to_vec();
        Box::pin(async move { Ok(Outcome::Ok(bytes)) })
    }
}

/// Inline host that always yields `Pending` on its first poll.
struct PendingInlineHost;

impl PendingInlineHost {
    const PLUGIN_ID: &'static str = "test.inline.pending";
}

impl Dispatch for PendingInlineHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(ParkOnce { parked: false })
    }
}

struct ParkOnce {
    parked: bool,
}

impl Future for ParkOnce {
    type Output = Result<Outcome, istmo_core::DispatchError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        if self.parked {
            Poll::Ready(Ok(Outcome::Ok(thread_id_as_u64().to_le_bytes().to_vec())))
        } else {
            self.parked = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    }
}

fn thread_id_as_u64() -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    thread::current().id().hash(&mut hasher);
    hasher.finish()
}

#[test]
fn inline_dispatch_runs_on_caller_thread() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);

    let thread_tag = Arc::new(AtomicU64::new(0));
    rt.register_host(InlineEcho {
        plugin_id: "test.inline.echo",
        thread_tag: Arc::clone(&thread_tag),
    });

    let caller_tag = thread_id_as_u64();
    let handle = rt
        .call("test.inline.echo", None, "ping", b"hello".to_vec())
        .expect("call");
    let result = handle.recv_blocking().expect("recv").expect("ok");
    assert_eq!(result, b"hello".to_vec());

    assert_eq!(
        thread_tag.load(Ordering::SeqCst),
        caller_tag,
        "inline host ran on a different thread",
    );
}

#[test]
fn inline_dispatch_finishes_waiting_hosts_on_a_worker() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);

    rt.register_host(PendingInlineHost);

    let handle = rt
        .call(PendingInlineHost::PLUGIN_ID, None, "waits", Vec::new())
        .expect("call");
    let bytes = handle.recv_blocking().expect("recv").expect("answered");
    let finished_on = u64::from_le_bytes(bytes.try_into().expect("thread tag"));
    assert_ne!(
        finished_on,
        thread_id_as_u64(),
        "the caller must not wait for a host that waits"
    );
}

/// Host whose calls block the thread serving them: the default mode.
struct BlockingHost;

impl Dispatch for BlockingHost {
    fn plugin_id(&self) -> &'static str {
        "test.blocking"
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async {
            thread::sleep(Duration::from_millis(50));
            Ok(Outcome::Ok(
                thread::current().name().unwrap_or("").as_bytes().to_vec(),
            ))
        })
    }
}

#[test]
fn isolated_calls_block_persistent_workers_not_the_caller() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);
    rt.register_host(BlockingHost);

    // Sixteen calls blocking at once all run concurrently.
    let started = std::time::Instant::now();
    let handles: Vec<_> = (0..16)
        .map(|_| {
            rt.call("test.blocking", None, "m", Vec::new())
                .expect("call")
        })
        .collect();
    assert!(
        started.elapsed() < Duration::from_millis(40),
        "issuing the calls must not wait for them"
    );
    for handle in handles {
        let name = handle.recv_blocking().expect("recv").expect("ok");
        assert!(
            String::from_utf8_lossy(&name).starts_with("istmo-blocking-"),
            "served by {:?}",
            String::from_utf8_lossy(&name)
        );
    }
    assert!(started.elapsed() < Duration::from_millis(16 * 50));
}

/// Generated-host style: a blocking method among inline ones.
struct MixedHost;

impl Dispatch for MixedHost {
    fn plugin_id(&self) -> &'static str {
        "test.mixed"
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn method_mode(&self, method: &str) -> DispatchMode {
        if method == "slow" {
            DispatchMode::Isolated
        } else {
            DispatchMode::Inline
        }
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let tag = thread_id_as_u64();
        Box::pin(async move { Ok(Outcome::Ok(tag.to_le_bytes().to_vec())) })
    }
}

#[test]
fn method_mode_picks_the_thread_per_method() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);
    rt.register_host(MixedHost);
    let served_on = |method: &str| {
        let bytes = rt
            .call("test.mixed", None, method, Vec::new())
            .expect("call")
            .recv_blocking()
            .expect("recv")
            .expect("ok");
        u64::from_le_bytes(bytes.try_into().expect("tag"))
    };
    assert_eq!(served_on("fast"), thread_id_as_u64());
    assert_ne!(served_on("slow"), thread_id_as_u64());
}

/// Inline host that panics on its first poll.
struct PanicInlineHost;

impl PanicInlineHost {
    const PLUGIN_ID: &'static str = "test.inline.panic";
}

impl Dispatch for PanicInlineHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async { panic!("inline host blew up") })
    }
}

/// Inline host that returns a stream of a few items.
struct StreamingInlineHost;

impl StreamingInlineHost {
    const PLUGIN_ID: &'static str = "test.inline.stream";
}

impl Dispatch for StreamingInlineHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async {
            let (tx, rx) = flume_bounded();
            // Fill the channel before the pump attaches; the sender
            // drops here, so the pump sees the items in order and then
            // a clean end-of-stream.
            let _ = tx.send(Ok(b"first".to_vec()));
            let _ = tx.send(Ok(b"second".to_vec()));
            let _ = tx.send(Ok(b"third".to_vec()));
            drop(tx);
            Ok(Outcome::StreamOpened(rx))
        })
    }
}

fn flume_bounded() -> (
    flume::Sender<istmo_core::HostedStreamItem>,
    flume::Receiver<istmo_core::HostedStreamItem>,
) {
    flume::bounded(8)
}

#[test]
fn inline_dispatch_catches_panics() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);

    rt.register_host(PanicInlineHost);

    let handle = rt
        .call(PanicInlineHost::PLUGIN_ID, None, "boom", Vec::new())
        .expect("call");
    let bytes = handle
        .recv_blocking()
        .expect("recv")
        .expect_err("expected dispatch error");
    let message = String::from_utf8_lossy(&bytes).to_string();
    assert!(
        message.contains("panic"),
        "unexpected error payload: {message:?}",
    );
}

#[test]
fn inline_dispatch_streams_through_pump() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    drop(init.outbound);

    rt.register_host(StreamingInlineHost);

    let stream = rt
        .stream(
            StreamingInlineHost::PLUGIN_ID,
            None,
            "subscribe",
            Vec::new(),
            4,
        )
        .expect("open");
    let mut items = Vec::new();
    while let StreamMessage::Event(bytes) = stream.recv().expect("recv") {
        items.push(bytes);
    }
    assert_eq!(
        items,
        vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec(),],
    );
}

#[test]
fn inline_dispatch_inbound_respond_via_outbound() {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let outbound = init.outbound;

    let thread_tag = Arc::new(AtomicU64::new(0));
    rt.register_host(InlineEcho {
        plugin_id: "test.inline.peer",
        thread_tag,
    });

    let call_id = CallId::new(42);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "test.inline.peer".to_owned(),
        instance_id: None,
        method: "ping".to_owned(),
        payload: b"peer-hello".to_vec(),
    }))
    .expect("inbound");

    let reply = outbound
        .recv_timeout(Duration::from_secs(2))
        .expect("outbound");
    match reply.frame {
        Frame::Respond {
            call_id: id,
            result,
        } => {
            assert_eq!(id, call_id);
            assert_eq!(result.expect("ok"), b"peer-hello".to_vec());
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}
