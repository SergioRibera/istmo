//! `Runtime::serve_peer_call`: answers ready before the call returns
//! come back directly; every other answer still goes out as a frame.

use std::time::Duration;

use istmo_core::{
    CallId, CancelToken, Dispatch, DispatchFuture, DispatchMode, Envelope, Frame, InstanceId,
    Outcome, Runtime,
};

struct Host {
    mode: DispatchMode,
}

impl Dispatch for Host {
    fn plugin_id(&self) -> &'static str {
        "test.direct"
    }

    fn mode(&self) -> DispatchMode {
        self.mode
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            match method {
                "echo" => Ok(Outcome::Ok(payload.to_vec())),
                "fail" => Ok(Outcome::DomainError(vec![0xee])),
                "wait" => {
                    let (tx, rx) = flume::bounded(1);
                    std::thread::spawn(move || {
                        std::thread::sleep(Duration::from_millis(10));
                        let _ = tx.send(());
                    });
                    let _ = rx.recv_async().await;
                    Ok(Outcome::Ok(b"late".to_vec()))
                }
                other => Err(istmo_core::DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

fn runtime(mode: DispatchMode) -> istmo_core::RuntimeInit {
    let init = Runtime::mock();
    init.runtime.register_host(Host { mode });
    init
}

fn respond(outbound: &flume::Receiver<Envelope>) -> (CallId, istmo_core::CallResult) {
    match outbound
        .recv_timeout(Duration::from_secs(5))
        .expect("respond frame")
        .frame
    {
        Frame::Respond { call_id, result } => (call_id, result),
        other => panic!("unexpected frame {other:?}"),
    }
}

#[test]
fn an_inline_answer_comes_back_directly() {
    let init = runtime(DispatchMode::Inline);
    let reply =
        init.runtime
            .serve_peer_call(CallId(1), "test.direct", None, "echo".into(), vec![1, 2]);
    assert_eq!(reply, Some(Ok(vec![1, 2])));
    let reply =
        init.runtime
            .serve_peer_call(CallId(2), "test.direct", None, "fail".into(), Vec::new());
    assert_eq!(reply, Some(Err(vec![0xee])));
    assert!(
        init.outbound.try_recv().is_err(),
        "no frame for a direct answer"
    );
}

#[test]
fn a_host_that_waits_answers_through_a_frame() {
    let init = runtime(DispatchMode::Inline);
    let reply =
        init.runtime
            .serve_peer_call(CallId(3), "test.direct", None, "wait".into(), Vec::new());
    assert_eq!(reply, None);
    assert_eq!(respond(&init.outbound), (CallId(3), Ok(b"late".to_vec())));
}

#[test]
fn a_blocking_host_answers_through_a_frame() {
    let init = runtime(DispatchMode::Isolated);
    let reply =
        init.runtime
            .serve_peer_call(CallId(4), "test.direct", None, "echo".into(), vec![9]);
    assert_eq!(reply, None);
    assert_eq!(respond(&init.outbound), (CallId(4), Ok(vec![9])));
}

#[test]
fn unhosted_and_bridged_calls_answer_through_frames() {
    let init = runtime(DispatchMode::Inline);
    let reply =
        init.runtime
            .serve_peer_call(CallId(5), "test.nobody", None, "echo".into(), Vec::new());
    assert_eq!(reply, None);
    let (call_id, result) = respond(&init.outbound);
    assert_eq!(call_id, CallId(5));
    assert!(result.is_err());

    // Marked as coming from another process: the sink answers it.
    let (sink_tx, sink_rx) = flume::unbounded();
    init.runtime
        .install_remote_envelope_sink(std::sync::Arc::new(move |bytes| {
            let _ = sink_tx.send(bytes);
        }));
    init.runtime.mark_call_remote(CallId(6));
    let reply =
        init.runtime
            .serve_peer_call(CallId(6), "test.direct", None, "echo".into(), vec![7]);
    assert_eq!(reply, None);
    let bytes = sink_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("sink frame");
    assert!(matches!(
        Envelope::from_wire_bytes(&bytes).expect("decode").frame,
        Frame::Respond { call_id: CallId(6), result: Ok(ref p) } if *p == [7]
    ));
}

#[test]
fn a_cancel_after_a_direct_answer_is_harmless() {
    let init = runtime(DispatchMode::Inline);
    let reply =
        init.runtime
            .serve_peer_call(CallId(8), "test.direct", None, "echo".into(), vec![3]);
    assert_eq!(reply, Some(Ok(vec![3])));
    init.runtime
        .dispatch_inbound(Envelope::new(Frame::Cancel { call_id: CallId(8) }))
        .expect("cancel");
    // The call left the hosted table: a fresh call with the same id runs.
    let reply =
        init.runtime
            .serve_peer_call(CallId(8), "test.direct", None, "echo".into(), vec![4]);
    assert_eq!(reply, Some(Ok(vec![4])));
}
