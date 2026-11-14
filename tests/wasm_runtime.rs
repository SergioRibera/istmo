//! The runtime on `wasm32-unknown-unknown`: no threads, hosted work runs
//! on the JavaScript event loop.
//!
//! Run with `wasm-bindgen-test-runner` as the target runner:
//! `cargo test --target wasm32-unknown-unknown --test wasm_runtime`.
#![cfg(all(target_arch = "wasm32", target_os = "unknown"))]

use istmo::{Runtime, StreamItem};
use wasm_bindgen_test::wasm_bindgen_test;

#[istmo::plugin(name = "test.wasm")]
pub trait Wasm {
    async fn echo(&self, text: String) -> String;

    #[istmo::stream]
    fn count_up(&self, to: u32) -> u32;
}

struct WasmImpl;

impl Wasm for WasmImpl {
    async fn echo(&self, text: String) -> String {
        text
    }

    fn count_up(&self, to: u32) -> flume::Receiver<u32> {
        let (tx, rx) = flume::unbounded();
        for i in 0..to {
            tx.send(i).unwrap();
        }
        rx
    }
}

fn client() -> WasmClient {
    let rt = Runtime::mock().host(WasmHost::new(WasmImpl)).runtime;
    WasmClient::from_runtime(&rt).unwrap()
}

#[wasm_bindgen_test]
async fn hosted_call_answers() {
    assert_eq!(client().echo("hola".to_owned()).await.unwrap(), "hola");
}

#[wasm_bindgen_test]
async fn hosted_stream_delivers_every_item_then_ends() {
    let client = client();
    let stream = client.count_up(3).unwrap();
    let mut items = Vec::new();
    loop {
        match stream.recv_async().await.unwrap() {
            StreamItem::Event(item) => items.push(item),
            StreamItem::Completed => break,
            other => panic!("unexpected end: {other:?}"),
        }
    }
    assert_eq!(items, [0, 1, 2]);
}

#[wasm_bindgen_test]
fn wall_clock_reads_the_browser() {
    let rt = Runtime::mock().runtime;
    let info = istmo::plugins::SystemInfoClient::from_runtime(&rt).unwrap();
    assert!(info.now_utc_millis() > 1_700_000_000_000);
}
