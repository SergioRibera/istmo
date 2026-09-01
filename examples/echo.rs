//! Toy plugin demonstrating `#[istmo::plugin]` + `#[istmo::message]`.
//!
//! Run with `cargo run --example echo`. The example wires the generated
//! client to a mock backend that lives in the same process; no JNI / iOS
//! bindings are exercised here.

use std::thread;

use istmo::{Envelope, Frame, Runtime, codec};

#[istmo::message]
#[derive(Debug)]
pub struct EchoError {
    pub reason: String,
}

#[istmo::plugin(name = "com.example.echo")]
pub trait Echo {
    async fn ping(&self, msg: String) -> Result<String, EchoError>;

    async fn add(&self, a: i32, b: i32) -> i32;
}

fn main() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let outbound = init.outbound;

    thread::spawn(move || {
        while let Ok(envelope) = outbound.recv() {
            handle(&rt, envelope);
        }
    });

    let echo = Echo::from_runtime(&init.runtime);
    let reply = pollster::block_on(echo.ping("hello".to_owned())).expect("ping ok");
    println!("ping response: {reply}");

    let sum = pollster::block_on(echo.add(2, 3)).expect("add ok");
    println!("2 + 3 = {sum}");
}

fn handle(rt: &Runtime, envelope: Envelope) {
    let Frame::Call {
        call_id,
        method,
        payload,
        ..
    } = envelope.frame
    else {
        return;
    };
    let response = match method.as_str() {
        "ping" => {
            let ((msg,), _) = codec::decode::<(String,)>(&payload).expect("decode msg");
            Ok(codec::encode(&format!("pong: {msg}")).expect("encode reply"))
        }
        "add" => {
            let ((a, b), _) = codec::decode::<(i32, i32)>(&payload).expect("decode ab");
            Ok(codec::encode(&(a + b)).expect("encode sum"))
        }
        _ => Err(codec::encode(&"unknown method".to_owned()).expect("encode err")),
    };
    rt.dispatch_inbound(Envelope::new(Frame::Respond {
        call_id,
        result: response,
    }))
    .expect("dispatch response");
}
