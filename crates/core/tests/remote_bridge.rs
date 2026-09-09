//! End-to-end demo of a `:remote`-process bridge using only the public
//! `Envelope::to_wire_bytes` / `Envelope::from_wire_bytes` /
//! `Runtime::inject_wire_envelope` primitives.
//!
//! Two [`Runtime`] instances model the app process and the `:remote`
//! process. A pair of `std::thread` shuttles stands in for the real
//! AIDL/Binder bridge; each drains its side's outbound channel, filters
//! for envelopes addressed to a remote-hosted plugin, and injects the
//! bincoded bytes into the other runtime. No `unsafe`, no Android SDK
//! — the same shape a Kotlin bridge would take.
//!
//! Assertions cover:
//!
//! * A `Frame::Call` originated on the app side reaches the `:remote`
//!   dispatcher and its `Frame::Respond` returns via the bridge.
//! * A domain error (`Outcome::DomainError`) round-trips through the
//!   same path.
//! * The bridge is bidirectional — the app side hosts an unrelated
//!   plugin the `:remote` side calls into, and vice versa in the same
//!   test.

use std::collections::HashSet;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use flume::Receiver as FlumeReceiver;
use istmo_core::{
    CancelToken, Dispatch, DispatchFuture, Envelope, Frame, InstanceId, Outcome, Plugin, Runtime,
    codec,
};

// -- Two hosted plugins, one per side --------------------------------------

/// `RemoteHost` lives in the ":remote" process. The app process's client
/// side dispatches every Call frame targeting `PLUGIN_ID` here.
struct RemoteHost {
    calls: AtomicU32,
}

impl RemoteHost {
    const PLUGIN_ID: &'static str = "test.remote.echo";

    const fn new() -> Self {
        Self {
            calls: AtomicU32::new(0),
        }
    }
}

impl Plugin for RemoteHost {
    const PLUGIN_ID: &'static str = Self::PLUGIN_ID;
}

impl Dispatch for RemoteHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let m = method.to_owned();
        let p = payload.to_vec();
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            if m == "fail" {
                let bytes = codec::encode(&"remote domain error".to_owned()).unwrap();
                return Ok(Outcome::DomainError(bytes));
            }
            // Echo the payload back prefixed with the method name.
            let mut out = Vec::new();
            out.extend_from_slice(m.as_bytes());
            out.push(b':');
            out.extend_from_slice(&p);
            Ok(Outcome::Ok(out))
        })
    }
}

/// `AppHost` lives in the app process. The `:remote` process's client
/// side sends inbound Calls here through the reverse bridge lane.
struct AppHost;

impl AppHost {
    const PLUGIN_ID: &'static str = "test.app.notify";
}

impl Plugin for AppHost {
    const PLUGIN_ID: &'static str = Self::PLUGIN_ID;
}

impl Dispatch for AppHost {
    fn plugin_id(&self) -> &'static str {
        Self::PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        _method: &'a str,
        _payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move { Ok(Outcome::Ok(codec::encode(&"noted".to_owned()).unwrap())) })
    }
}

// -- Bridge --------------------------------------------------------------

/// Drains `src`'s outbound and forwards every envelope whose plugin id
/// is in `remote_plugins` to `dst` via `inject_wire_envelope`.
/// Non-matching envelopes are dropped (a real bridge would keep them for
/// its Kotlin `onCall` — here nothing else is listening, so dropping is
/// fine for the test). Returns when `src` closes.
fn shuttle(
    src: FlumeReceiver<Envelope>,
    dst: Arc<Runtime>,
    remote_plugins: HashSet<&'static str>,
    label: &'static str,
    forwarded: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<()> {
    thread::Builder::new()
        .name(format!("bridge-{label}"))
        .spawn(move || {
            while let Ok(env) = src.recv() {
                let plugin = match &env.frame {
                    Frame::Call { plugin_id, .. }
                    | Frame::Notify { plugin_id, .. }
                    | Frame::CreateInstance { plugin_id, .. } => Some(plugin_id.as_str()),
                    _ => None,
                };
                // Respond / Event / StreamEnd / Cancel / ReleaseNativeHandle
                // all carry ids issued by the OTHER side's runtime;
                // forwarding them unconditionally lets the reply half of
                // a cross-process Call land on the initiating runtime.
                let forward = plugin.is_none_or(|id| remote_plugins.contains(id));
                if !forward {
                    continue;
                }
                let bytes = match env.to_wire_bytes() {
                    Ok(b) => b,
                    Err(err) => {
                        eprintln!("bridge-{label}: encode failed: {err}");
                        continue;
                    }
                };
                if let Err(err) = dst.inject_wire_envelope(&bytes) {
                    eprintln!("bridge-{label}: inject failed: {err}");
                    continue;
                }
                if let Frame::Call { method, .. } = &env.frame {
                    forwarded.lock().unwrap().push(format!("{label}:{method}"));
                }
            }
        })
        .expect("spawn shuttle thread")
}

// -- The actual test -----------------------------------------------------

#[test]
fn app_to_remote_call_and_response_flow_over_bridge_bytes() {
    // App process: declares the remote-hosted plugin; hosts one of its own.
    let app_init = Runtime::mock();
    let app = app_init.runtime;
    let app_outbound = app_init.outbound;
    app.declare_plugin(RemoteHost::PLUGIN_ID);
    app.register_host(AppHost);

    // :remote process: hosts the remote plugin; declares AppHost as its own
    // client since it will initiate a call back into the app side too.
    let remote_init = Runtime::mock();
    let remote = remote_init.runtime;
    let remote_outbound = remote_init.outbound;
    remote.declare_plugin(AppHost::PLUGIN_ID);
    remote.register_host(RemoteHost::new());

    let mut app_remote_plugins = HashSet::new();
    app_remote_plugins.insert(RemoteHost::PLUGIN_ID);
    let mut remote_remote_plugins = HashSet::new();
    remote_remote_plugins.insert(AppHost::PLUGIN_ID);

    let forwarded = Arc::new(Mutex::new(Vec::<String>::new()));
    let _app_to_remote = shuttle(
        app_outbound,
        Arc::clone(&remote),
        app_remote_plugins,
        "a2r",
        Arc::clone(&forwarded),
    );
    let _remote_to_app = shuttle(
        remote_outbound,
        Arc::clone(&app),
        remote_remote_plugins,
        "r2a",
        Arc::clone(&forwarded),
    );

    // ---- App-side originates a call to the remote-hosted plugin --------
    let handle = app
        .call(
            RemoteHost::PLUGIN_ID,
            None,
            "hello",
            codec::encode(&()).unwrap(),
        )
        .expect("open call");
    let response = pollster::block_on(handle).expect("app-side call handle");
    match response {
        Ok(bytes) => {
            let echoed = String::from_utf8(bytes).expect("utf-8 echo");
            assert!(echoed.starts_with("hello:"), "got {echoed:?}");
        }
        Err(err) => panic!("expected Ok, got Err({} bytes)", err.len()),
    }

    // ---- Domain error round-trip --------------------------------------
    let handle = app
        .call(
            RemoteHost::PLUGIN_ID,
            None,
            "fail",
            codec::encode(&()).unwrap(),
        )
        .expect("open fail call");
    let response = pollster::block_on(handle).expect("app-side call handle");
    match response {
        Err(bytes) => {
            let (msg, _) = codec::decode::<String>(&bytes).expect("decode domain error");
            assert_eq!(msg, "remote domain error");
        }
        Ok(bytes) => panic!("expected Err, got Ok({} bytes)", bytes.len()),
    }

    // ---- Reverse direction: :remote calls the app-hosted plugin ------
    let handle = remote
        .call(AppHost::PLUGIN_ID, None, "notify", codec::encode(&()).unwrap())
        .expect("open reverse call");
    let response = pollster::block_on(handle).expect("remote-side call handle");
    let bytes = match response {
        Ok(b) => b,
        Err(err) => panic!("reverse expected Ok, got Err({} bytes)", err.len()),
    };
    let (msg, _) = codec::decode::<String>(&bytes).expect("decode noted");
    assert_eq!(msg, "noted");

    // Confirm shuttles routed the right Call envelopes.
    // Wait briefly for the shuttle threads to catch up; forwarded lock
    // may not have every entry when the runtime's dispatch thread runs
    // faster than the shuttle append.
    for _ in 0..1_000 {
        let f = forwarded.lock().unwrap();
        if f.iter().any(|s| s == "r2a:notify")
            && f.iter().any(|s| s == "a2r:hello")
            && f.iter().any(|s| s == "a2r:fail")
        {
            return;
        }
        drop(f);
        thread::sleep(Duration::from_millis(1));
    }
    let snapshot = forwarded.lock().unwrap().clone();
    panic!("expected all three forwards to land; saw {snapshot:?}");
}
