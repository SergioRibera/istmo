//! `Runtime::install_direct_peer`: calls to the peer go out on the
//! caller's thread while the outbound channel is drained, and never
//! overtake a frame queued before them.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use istmo_core::{
    CallId, DirectOutcome, DirectPeer, Envelope, Frame, InstanceId, Runtime, RuntimeInit,
};

/// Answers `echo` in place, takes `later` for an answer through a
/// frame, refuses `refuse`.
#[derive(Default)]
struct Peer {
    seen: Mutex<Vec<(u64, String, std::thread::ThreadId)>>,
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
        self.seen.lock().unwrap().push((
            call_id.get(),
            method.to_owned(),
            std::thread::current().id(),
        ));
        match method {
            "echo" => DirectOutcome::Answered(Ok(payload.to_vec())),
            "fail" => DirectOutcome::Answered(Err(vec![1])),
            "later" => DirectOutcome::Pending,
            _ => DirectOutcome::Unavailable,
        }
    }
}

fn runtime() -> (RuntimeInit, Arc<Peer>) {
    let init = Runtime::mock();
    let peer = Arc::new(Peer::default());
    init.runtime
        .install_direct_peer(Arc::clone(&peer) as Arc<dyn DirectPeer>);
    (init, peer)
}

#[test]
fn calls_are_answered_on_the_calling_thread() {
    let (init, peer) = runtime();
    let rt = &init.runtime;
    let reply = rt
        .call("peer.p", None, "echo", vec![5])
        .expect("call")
        .recv_blocking()
        .expect("recv");
    assert_eq!(reply, Ok(vec![5]));
    let reply = rt
        .call("peer.p", None, "fail", Vec::new())
        .expect("call")
        .recv_blocking()
        .expect("recv");
    assert_eq!(reply, Err(vec![1]));
    let seen = peer.seen.lock().unwrap().clone();
    assert!(
        seen.iter()
            .all(|(_, _, t)| *t == std::thread::current().id())
    );
    assert!(init.outbound.try_recv().is_err(), "nothing queued");
}

#[test]
fn a_pending_direct_call_is_answered_by_a_respond_frame() {
    let (init, _peer) = runtime();
    let handle = init
        .runtime
        .call("peer.p", None, "later", Vec::new())
        .expect("call");
    assert!(init.outbound.try_recv().is_err(), "the peer took it");
    init.runtime
        .dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: handle.call_id(),
            result: Ok(vec![9]),
        }))
        .expect("respond");
    assert_eq!(handle.recv_blocking().expect("recv"), Ok(vec![9]));
}

#[test]
fn a_refused_call_and_calls_behind_queued_frames_use_the_channel() {
    let (init, peer) = runtime();
    let rt = &init.runtime;
    let refused = rt.call("peer.p", None, "refuse", Vec::new()).expect("call");
    let queued = init
        .outbound
        .recv_timeout(Duration::from_secs(1))
        .expect("queued call");
    assert!(matches!(queued.frame, Frame::Call { call_id, .. } if call_id == refused.call_id()));
    // That frame was taken but not reported delivered: the next call
    // waits behind it in the channel.
    let behind = rt.call("peer.p", None, "echo", vec![1]).expect("call");
    assert!(matches!(
        init.outbound.try_recv().expect("behind").frame,
        Frame::Call { call_id, .. } if call_id == behind.call_id()
    ));
    rt.outbound_delivered();
    rt.outbound_delivered();
    let direct = rt
        .call("peer.p", None, "echo", vec![2])
        .expect("call")
        .recv_blocking()
        .expect("recv");
    assert_eq!(direct, Ok(vec![2]));
    assert_eq!(peer.seen.lock().unwrap().len(), 2, "refuse + the last echo");
    // Over-reporting never wraps the count.
    rt.outbound_delivered();
    let direct = rt
        .call("peer.p", None, "echo", vec![3])
        .expect("call")
        .recv_blocking()
        .expect("recv");
    assert_eq!(direct, Ok(vec![3]));
}

#[test]
fn remote_plugins_never_go_direct() {
    let (init, peer) = runtime();
    let rt = &init.runtime;
    rt.declare_remote_plugin("remote.p");
    let _remote = rt.call("remote.p", None, "echo", Vec::new()).expect("call");
    assert!(peer.seen.lock().unwrap().is_empty());
    assert!(init.outbound.try_recv().is_ok(), "no sink: the channel");
}

/// Takes every stream item directly.
#[derive(Default)]
struct EventPeer {
    items: Mutex<Vec<(u64, Vec<u8>)>>,
}

impl DirectPeer for EventPeer {
    fn call(
        &self,
        _call_id: CallId,
        _plugin_id: &str,
        _instance_id: Option<InstanceId>,
        _method: &str,
        _payload: &[u8],
    ) -> DirectOutcome {
        DirectOutcome::Unavailable
    }

    fn event(&self, stream_id: istmo_core::StreamId, payload: &[u8]) -> bool {
        self.items
            .lock()
            .unwrap()
            .push((stream_id.get(), payload.to_vec()));
        true
    }
}

struct Three;

impl istmo_core::Dispatch for Three {
    fn plugin_id(&self) -> &'static str {
        "test.three"
    }

    fn mode(&self) -> istmo_core::DispatchMode {
        istmo_core::DispatchMode::Inline
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: istmo_core::CancelToken,
    ) -> istmo_core::DispatchFuture<'a> {
        Box::pin(async {
            let (tx, rx) = flume::bounded(4);
            for i in 0..3u8 {
                tx.send(Ok(vec![i])).unwrap();
            }
            Ok(istmo_core::Outcome::StreamOpened(rx))
        })
    }
}

#[test]
fn peer_stream_items_go_straight_to_the_peer_and_the_end_follows_them() {
    let init = Runtime::mock();
    let peer = Arc::new(EventPeer::default());
    init.runtime
        .install_direct_peer(Arc::clone(&peer) as Arc<dyn DirectPeer>);
    init.runtime.register_host(Three);
    init.runtime
        .dispatch_inbound(Envelope::new(Frame::Call {
            call_id: CallId(11),
            plugin_id: "test.three".to_owned(),
            instance_id: None,
            method: "s".to_owned(),
            payload: Vec::new(),
        }))
        .expect("call");
    let end = init
        .outbound
        .recv_timeout(Duration::from_secs(5))
        .expect("end");
    assert!(
        matches!(end.frame, Frame::StreamEnd { .. }),
        "{:?}",
        end.frame
    );
    assert_eq!(
        *peer.items.lock().unwrap(),
        [(11, vec![0]), (11, vec![1]), (11, vec![2])]
    );
}
