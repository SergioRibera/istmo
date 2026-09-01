//! End-to-end verification of `#[istmo::plugin]` and `#[istmo::message]`.
//!
//! Each test spins up a mock backend on a background thread that services
//! whatever frames the generated client emits, then exercises the client.

use std::thread;

use istmo::{Envelope, Frame, Runtime, StreamEndReason, StreamItem, codec};

#[istmo::message]
#[derive(Debug, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

#[istmo::plugin(name = "com.example.echo")]
pub trait Echo {
    async fn ping(&self, msg: String) -> Result<String, EchoError>;

    async fn add(&self, a: i32, b: i32) -> i32;

    #[istmo::stream]
    fn ticks(&self, count: u32) -> u32;
}

#[test]
fn generated_client_round_trips_unary_calls() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        for _ in 0..2 {
            let envelope = outbound.recv().expect("call frame");
            let Frame::Call {
                call_id,
                method,
                payload,
                ..
            } = envelope.frame
            else {
                panic!("expected call");
            };
            let result = match method.as_str() {
                "ping" => {
                    let ((msg,), _) = codec::decode::<(String,)>(&payload).unwrap();
                    Ok(codec::encode(&format!("pong:{msg}")).unwrap())
                }
                "add" => {
                    let ((a, b), _) = codec::decode::<(i32, i32)>(&payload).unwrap();
                    Ok(codec::encode(&(a + b)).unwrap())
                }
                other => panic!("unexpected method {other}"),
            };
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Respond { call_id, result }))
                .unwrap();
        }
    });

    let echo = Echo::from_runtime(&rt);
    assert_eq!(
        pollster::block_on(echo.ping("hi".to_owned())).unwrap(),
        "pong:hi",
    );
    assert_eq!(pollster::block_on(echo.add(2, 3)).unwrap(), 5);
    backend.join().unwrap();
}

#[test]
fn generated_client_surfaces_domain_errors_as_plugin_error_bytes() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let envelope = outbound.recv().expect("call");
        let Frame::Call { call_id, .. } = envelope.frame else {
            panic!("expected call");
        };
        let error_payload = codec::encode(&EchoError {
            reason: "nope".to_owned(),
        })
        .unwrap();
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::Respond {
                call_id,
                result: Err(error_payload),
            }))
            .unwrap();
    });

    let echo = Echo::from_runtime(&rt);
    let err = pollster::block_on(echo.ping("nope".to_owned())).expect_err("should fail");
    let bytes = match err {
        istmo::IstmoError::PluginError { bytes } => bytes,
        other => panic!("expected PluginError, got {other:?}"),
    };
    let (decoded, _) = codec::decode::<EchoError>(&bytes).unwrap();
    assert_eq!(
        decoded,
        EchoError {
            reason: "nope".to_owned()
        }
    );
    backend.join().unwrap();
}

#[test]
fn generated_client_reads_stream_events() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    let backend_rt = rt.clone();
    let backend = thread::spawn(move || {
        let envelope = outbound.recv().expect("call");
        let Frame::Call {
            call_id, payload, ..
        } = envelope.frame
        else {
            panic!("expected call");
        };
        let ((count,), _) = codec::decode::<(u32,)>(&payload).unwrap();
        let stream_id = istmo::StreamId(call_id.get());
        for tick in 0..count {
            backend_rt
                .dispatch_inbound(Envelope::new(Frame::Event {
                    stream_id,
                    payload: codec::encode(&tick).unwrap(),
                }))
                .unwrap();
        }
        backend_rt
            .dispatch_inbound(Envelope::new(Frame::StreamEnd {
                stream_id,
                reason: StreamEndReason::Complete,
            }))
            .unwrap();
    });

    let echo = Echo::from_runtime(&rt);
    let stream = echo.ticks(3).expect("open");
    let mut collected = Vec::new();
    loop {
        match stream.recv().unwrap() {
            StreamItem::Event(v) => collected.push(v),
            StreamItem::Completed => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(collected, vec![0_u32, 1, 2]);
    backend.join().unwrap();
}
