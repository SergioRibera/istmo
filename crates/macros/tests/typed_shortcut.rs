//! End-to-end check of the plugin macro's typed in-process shortcut.
//!
//! Registers a hand-rolled host via the macro-generated `register`
//! helper, calls through the client, and verifies both the fast path
//! and the wire fallback behave the same.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use istmo::{IstmoError, Runtime};

#[istmo::message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MathError {
    Overflow,
}

impl core::fmt::Display for MathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("math error: overflow")
    }
}

impl std::error::Error for MathError {}

#[istmo::message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireMathError(pub String);

impl From<MathError> for WireMathError {
    fn from(err: MathError) -> Self {
        Self(err.to_string())
    }
}

impl core::fmt::Display for WireMathError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for WireMathError {}

#[istmo::plugin(name = "test.typed.math")]
pub trait Math {
    async fn add(&self, a: i32, b: i32) -> Result<i32, WireMathError>;
    async fn shift(&self, p: Point, dx: i32, dy: i32) -> Point;
}

struct Backend {
    calls: AtomicU64,
}

impl Backend {
    fn new() -> Self {
        Self {
            calls: AtomicU64::new(0),
        }
    }
}

impl Math for Backend {
    async fn add(&self, a: i32, b: i32) -> Result<i32, WireMathError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        a.checked_add(b).ok_or_else(|| MathError::Overflow.into())
    }

    async fn shift(&self, p: Point, dx: i32, dy: i32) -> Point {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Point {
            x: p.x + dx,
            y: p.y + dy,
        }
    }
}

#[test]
fn typed_shortcut_hits_backend_without_the_wire() {
    pollster::block_on(async {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;

    MathHost::new(Backend::new()).register(&rt);

    let client = MathClient::from_runtime(&rt).expect("client");
    let sum = client.add(2, 3).await.expect("add");
    assert_eq!(sum, 5);

    let point = client
        .shift(Point { x: 1, y: 2 }, 10, -3)
        .await
        .expect("shift");
    assert_eq!(point, Point { x: 11, y: -1 });

    // Fast path never produced an outbound envelope.
    assert!(outbound.is_empty());
    });
}

#[test]
fn typed_shortcut_domain_error_surfaces_as_plugin_error() {
    pollster::block_on(async {
    let init = Runtime::mock();
    let rt = init.runtime;
    drop(init.outbound);

    MathHost::new(Backend::new()).register(&rt);

    let client = MathClient::from_runtime(&rt).expect("client");
    let err = client.add(i32::MAX, 1).await.expect_err("overflow");
    match err {
        IstmoError::PluginError { bytes } => {
            // The error re-encoded through codec::encode so the caller
            // can decode it with the existing wire path.
            let (decoded, _) =
                istmo::codec::decode::<WireMathError>(&bytes).expect("decode err");
            assert_eq!(decoded, WireMathError("math error: overflow".to_owned()));
        }
        other => panic!("unexpected error: {other:?}"),
    }
    });
}

#[test]
fn typed_shortcut_host_panic_surfaces_as_plugin_error() {
    pollster::block_on(async {
        struct PanicBackend;

        impl Math for PanicBackend {
            async fn add(&self, _a: i32, _b: i32) -> Result<i32, WireMathError> {
                panic!("boom from typed host");
            }

            async fn shift(&self, p: Point, _dx: i32, _dy: i32) -> Point {
                p
            }
        }

        let init = Runtime::mock();
        let rt = init.runtime;
        drop(init.outbound);

        MathHost::new(PanicBackend).register(&rt);

        let client = MathClient::from_runtime(&rt).expect("client");
        let err = client.add(1, 2).await.expect_err("panic");
        match err {
            IstmoError::PluginError { bytes } => {
                let (decoded, _) =
                    istmo::codec::decode::<String>(&bytes).expect("decode panic marker");
                assert_eq!(decoded, "host panicked");
            }
            other => panic!("unexpected error: {other:?}"),
        }
    });
}

#[test]
fn wire_fallback_when_no_typed_shortcut() {
    pollster::block_on(async {
    let init = Runtime::mock();
    let rt: Arc<Runtime> = init.runtime;
    let _outbound = init.outbound;

    // Register only via Dispatch (wire path), not typed.
    rt.register_host(MathHost::new(Backend::new()));

    let client = MathClient::from_runtime(&rt).expect("client");
    let sum = client.add(7, 8).await.expect("add via wire");
    assert_eq!(sum, 15);
    });
}
