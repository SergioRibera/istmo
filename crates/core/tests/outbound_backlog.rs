//! The outbound backlog under concurrent senders and a draining
//! transport: it returns to zero once everything is delivered, and a
//! frame handed to the direct peer never overtakes one queued before it.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use istmo_core::{
    CallId, CancelToken, DirectOutcome, DirectPeer, Dispatch, DispatchFuture, DispatchMode, Frame,
    InstanceId, Outcome, Runtime, RuntimeInit, StreamId,
};

/// Calls and stream items in the order the peer received them, through
/// either route.
#[derive(Default)]
struct Peer {
    calls: Mutex<Vec<u64>>,
    events: Mutex<Vec<u32>>,
    /// Takes every other item directly; the rest go to the channel.
    take_event: AtomicBool,
    /// `probe` calls answered directly.
    direct_probes: AtomicUsize,
}

impl DirectPeer for Peer {
    fn call(
        &self,
        call_id: CallId,
        _plugin_id: &str,
        _instance_id: Option<InstanceId>,
        method: &str,
        payload: &[u8],
    ) -> DirectOutcome {
        match method {
            "probe" => {
                self.direct_probes.fetch_add(1, Ordering::AcqRel);
                DirectOutcome::Answered(Ok(Vec::new()))
            }
            "echo" => {
                self.calls.lock().unwrap().push(call_id.get());
                DirectOutcome::Answered(Ok(payload.to_vec()))
            }
            _ => DirectOutcome::Unavailable,
        }
    }

    fn event(&self, _stream_id: StreamId, payload: &[u8]) -> bool {
        if !self.take_event.fetch_xor(true, Ordering::AcqRel) {
            return false;
        }
        self.events.lock().unwrap().push(item(payload));
        true
    }
}

fn item(payload: &[u8]) -> u32 {
    u32::from_le_bytes(payload.try_into().expect("u32 item"))
}

fn runtime() -> (RuntimeInit, Arc<Peer>) {
    let init = Runtime::mock();
    let peer = Arc::new(Peer::default());
    init.runtime
        .install_direct_peer(Arc::clone(&peer) as Arc<dyn DirectPeer>);
    (init, peer)
}

/// A transport thread: records what it delivers, then reports it.
fn pump(init: &RuntimeInit, peer: &Arc<Peer>, stop: &Arc<AtomicBool>) -> JoinHandle<()> {
    let runtime = Arc::clone(&init.runtime);
    let outbound = init.outbound.clone();
    let peer = Arc::clone(peer);
    let stop = Arc::clone(stop);
    std::thread::spawn(move || {
        loop {
            match outbound.recv_timeout(Duration::from_millis(2)) {
                Ok(envelope) => {
                    match envelope.frame {
                        Frame::Call {
                            call_id, method, ..
                        } if method != "probe" => {
                            peer.calls.lock().unwrap().push(call_id.get());
                        }
                        Frame::Event { payload, .. } => {
                            peer.events.lock().unwrap().push(item(&payload));
                        }
                        _ => {}
                    }
                    runtime.outbound_delivered();
                }
                Err(_) if stop.load(Ordering::Acquire) => return,
                Err(_) => {}
            }
        }
    })
}

/// Wait until a call goes to the peer directly again: the backlog is
/// back at zero.
fn goes_direct_again(init: &RuntimeInit, peer: &Peer) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let before = peer.direct_probes.load(Ordering::Acquire);
        drop(
            init.runtime
                .call("peer.p", None, "probe", Vec::new())
                .expect("call"),
        );
        if peer.direct_probes.load(Ordering::Acquire) > before {
            return true;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    false
}

#[test]
fn the_backlog_drains_to_zero_under_concurrent_senders() {
    for _ in 0..10 {
        let (init, peer) = runtime();
        let stop = Arc::new(AtomicBool::new(false));
        let pump = pump(&init, &peer, &stop);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    for _ in 0..500 {
                        drop(
                            init.runtime
                                .call("peer.p", None, "refuse", Vec::new())
                                .expect("call"),
                        );
                    }
                });
            }
        });
        assert!(
            goes_direct_again(&init, &peer),
            "the backlog never returned to zero after the channel drained"
        );
        stop.store(true, Ordering::Release);
        pump.join().expect("pump");
        init.runtime.shutdown();
    }
}

#[test]
fn a_direct_call_never_overtakes_a_frame_queued_before_it() {
    let (init, peer) = runtime();
    let stop = Arc::new(AtomicBool::new(false));
    let pump = pump(&init, &peer, &stop);
    let per_sender = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            scope.spawn(|| {
                let mut mine = Vec::new();
                for round in 0..300 {
                    // A queued frame first, then a call that may go direct.
                    let method = if round % 2 == 0 { "refuse" } else { "echo" };
                    let handle = init
                        .runtime
                        .call("peer.p", None, method, Vec::new())
                        .expect("call");
                    mine.push(handle.call_id().get());
                    drop(handle);
                }
                per_sender.lock().unwrap().push(mine);
            });
        }
    });
    let per_sender = per_sender.into_inner().unwrap();
    assert!(goes_direct_again(&init, &peer));
    stop.store(true, Ordering::Release);
    pump.join().expect("pump");
    let delivered = peer.calls.lock().unwrap().clone();
    for mine in per_sender {
        let mine: HashSet<u64> = mine.into_iter().collect();
        let order: Vec<u64> = delivered
            .iter()
            .copied()
            .filter(|id| mine.contains(id))
            .collect();
        assert_eq!(order.len(), mine.len(), "every call reached the peer once");
        assert!(
            order.is_sorted(),
            "a sender's calls reached the peer out of order: {order:?}"
        );
    }
}

/// `count` items, as fast as the stream takes them.
struct Items(u32);

impl Dispatch for Items {
    fn plugin_id(&self) -> &'static str {
        "test.items"
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
        let count = self.0;
        Box::pin(async move {
            let (tx, rx) = flume::bounded(4);
            std::thread::spawn(move || {
                for i in 0..count {
                    if tx.send(Ok(i.to_le_bytes().to_vec())).is_err() {
                        return;
                    }
                }
            });
            Ok(Outcome::StreamOpened(rx))
        })
    }
}

#[test]
fn stream_items_reach_the_peer_in_order_through_both_routes() {
    let count = 3000;
    let (init, peer) = runtime();
    init.runtime.register_host(Items(count));
    let stop = Arc::new(AtomicBool::new(false));
    let pump = pump(&init, &peer, &stop);
    // Other senders keep the backlog moving while the stream runs.
    let noise_stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        for _ in 0..4 {
            scope.spawn(|| {
                while !noise_stop.load(Ordering::Acquire) {
                    drop(
                        init.runtime
                            .call("peer.p", None, "refuse", Vec::new())
                            .expect("call"),
                    );
                }
            });
        }
        init.runtime.serve_peer_stream(
            CallId(u64::MAX / 2),
            "test.items",
            None,
            "items".into(),
            Vec::new(),
            u32::MAX,
        );
        let deadline = Instant::now() + Duration::from_secs(20);
        while Instant::now() < deadline {
            let len = peer.events.lock().unwrap().len();
            if len >= count as usize {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
        noise_stop.store(true, Ordering::Release);
    });
    stop.store(true, Ordering::Release);
    pump.join().expect("pump");
    let events = peer.events.lock().unwrap().clone();
    assert_eq!(events.len(), count as usize, "every item arrived");
    assert_eq!(events, (0..count).collect::<Vec<_>>(), "in order");
}
