//! Peer streams opened with a credit window.

use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, DispatchMode, Envelope, Frame, InstanceId,
    Outcome, Runtime, StreamEndReason, StreamId,
};

/// `flood` produces 1000 items as fast as the stream takes them.
struct Flood;

impl Dispatch for Flood {
    fn plugin_id(&self) -> &'static str {
        "test.flood"
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
            let (tx, rx) = flume::bounded(8);
            std::thread::spawn(move || {
                for i in 0..1000u32 {
                    if tx.send(Ok(i.to_le_bytes().to_vec())).is_err() {
                        return;
                    }
                }
            });
            Ok(Outcome::StreamOpened(rx))
        })
    }
}

fn events(outbound: &flume::Receiver<Envelope>, until: Duration) -> (Vec<u32>, bool) {
    let mut items = Vec::new();
    let mut ended = false;
    while let Ok(envelope) = outbound.recv_timeout(until) {
        match envelope.frame {
            Frame::Event { payload, .. } => {
                items.push(u32::from_le_bytes(payload.try_into().expect("u32")));
            }
            Frame::StreamEnd {
                reason: StreamEndReason::Complete,
                ..
            } => {
                ended = true;
                break;
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    (items, ended)
}

#[test]
fn a_peer_stream_never_runs_ahead_of_its_credit() {
    let init = Runtime::mock();
    init.runtime.register_host(Flood);
    init.runtime.serve_peer_stream(
        CallId(1),
        "test.flood",
        None,
        "flood".into(),
        Vec::new(),
        16,
    );
    let (first, ended) = events(&init.outbound, Duration::from_millis(100));
    assert_eq!(first, (0..16).collect::<Vec<_>>(), "exactly the window");
    assert!(!ended);

    let mut all = first;
    let mut ended = false;
    while !ended {
        init.runtime.grant_stream_credit(StreamId(1), 100);
        let (more, end) = events(&init.outbound, Duration::from_millis(100));
        assert!(more.len() <= 100, "never past the credit");
        assert!(
            end || more.len() == 100,
            "a grant is used up while items remain"
        );
        all.extend(more);
        ended = end;
    }
    assert_eq!(all, (0..1000).collect::<Vec<_>>(), "in order, nothing lost");
    // Credit for an ended stream is ignored.
    init.runtime.grant_stream_credit(StreamId(1), 5);
}

#[test]
fn a_cancel_frees_a_stream_waiting_for_credit() {
    let init = Runtime::mock();
    init.runtime.register_host(Flood);
    init.runtime
        .serve_peer_stream(CallId(2), "test.flood", None, "flood".into(), Vec::new(), 1);
    let (first, _) = events(&init.outbound, Duration::from_millis(100));
    assert_eq!(first, [0]);
    init.runtime
        .dispatch_inbound(Envelope::new(Frame::Cancel { call_id: CallId(2) }))
        .expect("cancel");
    let end = init
        .outbound
        .recv_timeout(Duration::from_secs(2))
        .expect("end");
    assert!(matches!(
        end.frame,
        Frame::StreamEnd {
            reason: StreamEndReason::Cancelled,
            ..
        }
    ));
}

#[test]
fn an_unhosted_peer_stream_is_refused() {
    let init = Runtime::mock();
    init.runtime
        .serve_peer_stream(CallId(3), "test.none", None, "s".into(), Vec::new(), 4);
    let answer = init
        .outbound
        .recv_timeout(Duration::from_secs(1))
        .expect("answer");
    assert!(matches!(
        answer.frame,
        Frame::Respond {
            call_id: CallId(3),
            result: Err(_)
        }
    ));
}
