//! Toy `Echo` plugin demonstrating the settled pattern:
//!
//! ```ignore
//! struct MockX; impl X for MockX { ... }
//! let init = Runtime::mock().expects::<XClient>().host(XHost::new(MockX)).finish();
//! ```
//!
//! No manual `outbound.recv()` loop, no string-matched dispatch, no codec
//! at user level — the generated `XHost` dispatcher decodes / encodes for
//! you.
//!
//! Run with `cargo run --example echo`.

use istmo::{Runtime, message, plugin};

#[message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EchoError {
    pub reason: String,
}

#[plugin(name = "com.example.echo")]
pub trait Echo {
    async fn ping(&self, msg: String) -> Result<String, EchoError>;
    async fn add(&self, a: i32, b: i32) -> i32;
}

#[derive(Debug, Default)]
pub struct MockEcho;

impl Echo for MockEcho {
    async fn ping(&self, msg: String) -> Result<String, EchoError> {
        Ok(format!("pong: {msg}"))
    }

    async fn add(&self, a: i32, b: i32) -> i32 {
        a + b
    }
}

fn main() {
    let init = Runtime::mock()
        .expects::<EchoClient>()
        .host(EchoHost::new(MockEcho))
        .finish();

    let echo = EchoClient::from_runtime(&init.runtime).expect("declared");
    let reply = pollster::block_on(echo.ping("hello".to_owned())).expect("ping ok");
    println!("ping response: {reply}");

    let sum = pollster::block_on(echo.add(2, 3)).expect("add ok");
    println!("2 + 3 = {sum}");
}
