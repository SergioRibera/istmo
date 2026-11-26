//! Frames queued behind a full outbound channel drain without the async
//! workers. Its own test binary: it blocks every async worker of the
//! process on purpose.

use std::sync::Arc;
use std::time::{Duration, Instant};

use istmo_core::{
    CancelToken, DEFAULT_OUTBOUND_CAPACITY, DeliveryScope, Dispatch, DispatchFuture, DispatchMode,
    InstanceId, Outcome, Runtime,
};

/// An inline host that, once on an async worker, blocks it until
/// released: a contract violation, but what a host doing "a bit of sync
/// I/O" does.
struct Hog {
    resume: flume::Receiver<()>,
    release: flume::Receiver<()>,
}

impl Dispatch for Hog {
    fn plugin_id(&self) -> &'static str {
        "test.hog"
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
        let resume = self.resume.clone();
        let release = self.release.clone();
        Box::pin(async move {
            let _ = resume.recv_async().await;
            let _ = release.recv();
            Ok(Outcome::Ok(Vec::new()))
        })
    }
}

#[test]
fn a_plain_sender_behind_queued_frames_needs_no_async_worker() {
    let init = Runtime::mock();
    let (resume_tx, resume_rx) = flume::unbounded();
    let (release_tx, release_rx) = flume::unbounded();
    init.runtime.register_host(Hog {
        resume: resume_rx,
        release: release_rx,
    });
    // More hogs than workers: every worker ends up blocked.
    let hogs = 8;
    let calls: Vec<_> = (0..hogs)
        .map(|_| {
            init.runtime
                .call("test.hog", None, "hog", Vec::new())
                .expect("call")
        })
        .collect();
    for _ in 0..hogs {
        resume_tx.send(()).expect("resume");
    }
    std::thread::sleep(Duration::from_millis(200));

    for _ in 0..DEFAULT_OUTBOUND_CAPACITY {
        init.runtime
            .notify("peer.p", None, "fill", Vec::new())
            .expect("room");
    }
    {
        let _pump = DeliveryScope::enter();
        init.runtime
            .notify("peer.p", None, "queued", Vec::new())
            .expect("queued");
    }
    let runtime = Arc::clone(&init.runtime);
    let plain = std::thread::spawn(move || runtime.notify("peer.p", None, "plain", Vec::new()));

    // The transport drains; the plain sender gets through although no
    // async worker is free.
    let deadline = Instant::now() + Duration::from_secs(5);
    while !plain.is_finished() && Instant::now() < deadline {
        let _ = init.outbound.recv_timeout(Duration::from_millis(10));
    }
    let finished = plain.is_finished();
    for _ in 0..hogs {
        release_tx.send(()).expect("release");
    }
    assert!(finished, "the plain sender waited for a free async worker");
    plain.join().expect("plain").expect("sent");
    drop(calls);
    init.runtime.shutdown();
}
