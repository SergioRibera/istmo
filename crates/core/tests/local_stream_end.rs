//! A Rust-hosted stream's end waits for a full local consumer without
//! holding an async worker. Its own test binary: it parks more stream
//! ends than the process has workers.

use std::time::{Duration, Instant};

use istmo_core::{
    CancelToken, Dispatch, DispatchFuture, DispatchMode, InstanceId, Outcome, Runtime,
    StreamEndReason, StreamMessage,
};

/// `stream`: one item, then the end, both sent from an async worker.
/// `wait`: answers once a timer thread wakes it, which needs a free
/// worker to resume on.
struct OneItem;

/// Resolves once another thread sent, so the rest of the future runs on
/// an async worker.
async fn hop_to_a_worker(after: Duration) {
    let (tx, rx) = flume::bounded::<()>(1);
    std::thread::spawn(move || {
        std::thread::sleep(after);
        let _ = tx.send(());
    });
    let _ = rx.recv_async().await;
}

impl Dispatch for OneItem {
    fn plugin_id(&self) -> &'static str {
        "test.one"
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            if method == "wait" {
                hop_to_a_worker(Duration::from_millis(10)).await;
                return Ok(Outcome::Ok(Vec::new()));
            }
            hop_to_a_worker(Duration::ZERO).await;
            let (tx, rx) = flume::unbounded();
            tx.send(Ok(vec![1])).expect("receiver alive");
            drop(tx);
            Ok(Outcome::StreamOpened(rx))
        })
    }
}

#[test]
fn stream_ends_to_full_consumers_leave_the_workers_free() {
    let init = Runtime::mock();
    init.runtime.register_host(OneItem);
    // More streams than workers, each with its one slot taken by the
    // item nobody reads yet: every end waits for room.
    let streams: Vec<_> = (0..8)
        .map(|_| {
            init.runtime
                .stream("test.one", None, "stream", Vec::new(), 1)
                .expect("stream")
        })
        .collect();
    std::thread::sleep(Duration::from_millis(200));

    let call = init
        .runtime
        .call("test.one", None, "wait", Vec::new())
        .expect("call");
    let waiter = std::thread::spawn(move || call.recv_blocking());
    let deadline = Instant::now() + Duration::from_secs(5);
    while !waiter.is_finished() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let finished = waiter.is_finished();

    // Read either way, so a failure does not leave the call hanging.
    for stream in &streams {
        assert_eq!(stream.recv().expect("item"), StreamMessage::Event(vec![1]));
        assert_eq!(
            stream.recv().expect("end"),
            StreamMessage::End(StreamEndReason::Complete)
        );
    }
    assert!(
        finished,
        "a call that needs an async worker waited for stream consumers to read"
    );
    let answer = waiter.join().expect("waiter").expect("answered");
    assert_eq!(answer, Ok(Vec::new()));
    init.runtime.shutdown();
}
