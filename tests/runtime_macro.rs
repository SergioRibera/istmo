//! Verifies that `istmo::runtime!` emits a working `__istmo_configure_runtime`
//! function that wires expects/hosts onto a `RuntimeInit`.
//!
//! On non-android targets the macro emits only the configuration function;
//! the JNI `pub use` block is `#[cfg(target_os = "android")]`-gated.

use std::sync::Arc;

use istmo::{Envelope, Frame, IstmoError, Runtime};

#[istmo::message]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nothing;

#[istmo::plugin(name = "test.runtime.echo")]
pub trait RuntimeEcho {
    async fn ping(&self, msg: String) -> String;
}

#[istmo::plugin(name = "test.runtime.absent")]
pub trait RuntimeAbsent {
    async fn nope(&self) -> Nothing;
}

#[derive(Debug)]
pub struct EchoImpl;

impl RuntimeEcho for EchoImpl {
    async fn ping(&self, msg: String) -> String {
        format!("echo:{msg}")
    }
}

// The macro under test.
istmo::runtime!(
    plugins: [RuntimeAbsent],
    hosts:   [RuntimeEcho => EchoImpl],
);

fn wired_init() -> (Arc<Runtime>, flume::Receiver<Envelope>) {
    let init = __istmo_configure_runtime(Runtime::mock());
    (init.runtime, init.outbound)
}

#[test]
fn runtime_macro_enforces_declared_plugins() {
    let (rt, _outbound) = wired_init();

    // Declared → ok.
    RuntimeAbsentClient::from_runtime(&rt).expect("declared");

    // Not declared → PluginNotDeclared.
    let err = RuntimeEchoClient::from_runtime(&rt).expect_err("must reject undeclared");
    assert!(
        matches!(err, IstmoError::PluginNotDeclared("test.runtime.echo")),
        "got {err:?}",
    );
}

#[test]
fn runtime_macro_registers_host_dispatcher() {
    let (rt, outbound) = wired_init();

    // Submit an inbound Call to the hosted plugin.
    let call_id = istmo::CallId(500);
    rt.dispatch_inbound(Envelope::new(Frame::Call {
        call_id,
        plugin_id: "test.runtime.echo".to_owned(),
        instance_id: None,
        method: "ping".to_owned(),
        payload: istmo::codec::encode(&("bonjour".to_owned(),)).expect("encode"),
    }))
    .expect("dispatch call");

    // Await the Respond.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    while std::time::Instant::now() < deadline {
        if let Ok(env) = outbound.recv_timeout(std::time::Duration::from_millis(50)) {
            if let Frame::Respond {
                call_id: cid,
                result: Ok(bytes),
            } = env.frame
            {
                if cid == call_id {
                    let (reply, _) =
                        istmo::codec::decode::<String>(&bytes).expect("decode reply");
                    assert_eq!(reply, "echo:bonjour");
                    return;
                }
            }
        }
    }
    panic!("timed out waiting for Respond");
}
