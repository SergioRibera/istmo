//! Tests for the typed-shortcut slot on [`Runtime`].
//!
//! Mirrors the shape the plugin macro will emit: a hidden trait
//! `<Trait>Typed` with one typed method per plugin method, registered
//! alongside the wire-path dispatcher.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use istmo_core::Runtime;

/// Hand-rolled stand-in for the macro-emitted `MathTyped`.
trait MathTyped: Send + Sync + 'static {
    fn add(&self, a: i64, b: i64) -> i64;
    fn mul(&self, a: i64, b: i64) -> i64;
}

struct MathImpl {
    calls: AtomicU64,
}

impl MathTyped for MathImpl {
    fn add(&self, a: i64, b: i64) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        a + b
    }

    fn mul(&self, a: i64, b: i64) -> i64 {
        self.calls.fetch_add(1, Ordering::SeqCst);
        a * b
    }
}

#[test]
fn typed_shortcut_roundtrips_without_touching_the_wire() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let outbound = init.outbound;

    let backend: Arc<MathImpl> = Arc::new(MathImpl {
        calls: AtomicU64::new(0),
    });
    let typed: Arc<dyn MathTyped> = backend.clone();
    rt.register_typed_host("test.math", typed);

    // Bind the return type explicitly so a regression that returned
    // `Arc<Arc<dyn MathTyped>>` would fail to compile here.
    let got: Arc<dyn MathTyped> = rt
        .typed_host::<dyn MathTyped>("test.math")
        .expect("slot registered");
    assert_eq!(got.add(2, 3), 5);
    assert_eq!(got.mul(4, 6), 24);
    assert_eq!(backend.calls.load(Ordering::SeqCst), 2);

    // Nothing reached the outbound channel: the typed path never built
    // an envelope.
    assert!(outbound.is_empty());
}

#[test]
fn typed_shortcut_missing_plugin_returns_none() {
    let init = Runtime::mock();
    let rt = init.runtime;
    drop(init.outbound);

    assert!(rt.typed_host::<dyn MathTyped>("test.unknown").is_none());
}

#[test]
fn typed_shortcut_wrong_trait_type_returns_none() {
    let init = Runtime::mock();
    let rt = init.runtime;
    drop(init.outbound);

    let impl_: Arc<dyn MathTyped> = Arc::new(MathImpl {
        calls: AtomicU64::new(0),
    });
    rt.register_typed_host("test.math", impl_);

    // Downcast to a different trait object should fail cleanly.
    trait OtherTyped: Send + Sync + 'static {
        #[allow(dead_code)]
        fn x(&self) -> i64;
    }
    assert!(rt.typed_host::<dyn OtherTyped>("test.math").is_none());
}

#[test]
fn typed_shortcut_cleared_on_shutdown() {
    let init = Runtime::mock();
    let rt = init.runtime;
    drop(init.outbound);

    let impl_: Arc<dyn MathTyped> = Arc::new(MathImpl {
        calls: AtomicU64::new(0),
    });
    rt.register_typed_host("test.math", impl_);
    assert!(rt.typed_host::<dyn MathTyped>("test.math").is_some());

    rt.shutdown();
    assert!(rt.typed_host::<dyn MathTyped>("test.math").is_none());
}
