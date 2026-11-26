//! `HostCtx` arguments, origin checks on frame-sink methods, the
//! `#[istmo::blocking]` opt-out and the `shortcut = false` /
//! `#[wire_only]` opt-outs of the typed shortcut.

use std::sync::Arc;
use std::thread::ThreadId;

use istmo::{
    CallOrigin, CancelToken, Dispatch, DispatchError, FrameSinkId, HostCtx, IstmoError,
    NativeBuffer, Outcome, Runtime, SinkPolicy, codec,
};

#[istmo::plugin(name = "test.ctx.camera")]
pub trait Camera {
    /// Feed `frames` until the sink ends.
    async fn start(&self, ctx: HostCtx, frames: FrameSinkId<u32>) -> bool;
    /// Who called, as the host sees it.
    async fn origin(&self, ctx: HostCtx) -> String;
    /// The thread the host body runs on.
    #[istmo::blocking]
    async fn blocking_thread(&self) -> String;
    /// The thread the host body runs on, through the shortcut.
    async fn thread(&self) -> String;
    /// An optional second sink is a legal argument.
    async fn maybe(&self, ctx: HostCtx, frames: Option<FrameSinkId<u32>>) -> bool;
}

#[istmo::plugin(name = "test.ctx.wire", shortcut = false)]
pub trait WireOnly {
    async fn ping(&self) -> u32;
}

struct Impl;

fn thread_name(id: ThreadId) -> String {
    format!("{id:?}")
}

impl Camera for Impl {
    async fn start(&self, ctx: HostCtx, frames: FrameSinkId<u32>) -> bool {
        let Ok(sender) = ctx.frame_sender(frames) else {
            return false;
        };
        std::thread::spawn(move || {
            for n in 0..3 {
                if sender
                    .push(n, NativeBuffer::from_bytes(vec![0u8; 4]))
                    .is_err()
                {
                    return;
                }
            }
            sender.close();
        });
        true
    }

    async fn origin(&self, ctx: HostCtx) -> String {
        format!("{:?}", ctx.origin())
    }

    async fn blocking_thread(&self) -> String {
        thread_name(std::thread::current().id())
    }

    async fn thread(&self) -> String {
        thread_name(std::thread::current().id())
    }

    async fn maybe(&self, _ctx: HostCtx, frames: Option<FrameSinkId<u32>>) -> bool {
        frames.is_some()
    }
}

impl WireOnly for Impl {
    async fn ping(&self) -> u32 {
        7
    }
}

#[test]
fn a_rust_host_feeds_a_sink_through_its_context() {
    let rt = Runtime::mock().runtime;
    rt.register_host(CameraHost::new(Impl));
    let client = CameraClient::from_runtime(&rt).expect("client");
    assert!(client.is_local());
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::queue(8))
        .expect("open");
    assert!(pollster::block_on(client.start(sink.id())).expect("start"));
    let got: Vec<u32> = std::iter::from_fn(|| sink.recv().map(|f| f.meta)).collect();
    assert_eq!(got, [0, 1, 2]);
    assert!(sink.end_reason().is_some_and(|r| r.is_completed()));
    assert_eq!(
        pollster::block_on(client.origin()).expect("origin"),
        "Local"
    );
    assert!(pollster::block_on(client.maybe(Some(sink.id()))).expect("maybe"));
}

#[test]
fn blocking_methods_run_on_a_blocking_worker() {
    let rt = Runtime::mock().runtime;
    rt.register_host(CameraHost::new(Impl));
    let client = CameraClient::from_runtime(&rt).expect("client");
    let here = thread_name(std::thread::current().id());
    assert_eq!(pollster::block_on(client.thread()).expect("thread"), here);
    assert_ne!(
        pollster::block_on(client.blocking_thread()).expect("thread"),
        here,
        "a #[istmo::blocking] method runs on a blocking worker"
    );
}

#[test]
fn generated_hosts_pick_a_mode_per_method() {
    let host = CameraHost::new(Impl);
    assert_eq!(host.method_mode("thread"), istmo::DispatchMode::Inline);
    assert_eq!(
        host.method_mode("blocking_thread"),
        istmo::DispatchMode::Isolated
    );
    assert_eq!(
        WireOnlyHost::new(Impl).method_mode("ping"),
        istmo::DispatchMode::Inline
    );
}

#[test]
fn peer_calls_run_inline_unless_blocking() {
    // A call delivered by the platform peer is served on the delivering
    // thread; a blocking method on a blocking worker.
    let init = Runtime::mock();
    let rt = init.runtime;
    rt.register_host(CameraHost::new(Impl));
    let here = thread_name(std::thread::current().id());
    let served_on = |call_id: u64, method: &str| {
        rt.dispatch_inbound(istmo::Envelope::new(istmo::Frame::Call {
            call_id: istmo::CallId(call_id),
            plugin_id: "test.ctx.camera".to_owned(),
            instance_id: None,
            method: method.to_owned(),
            payload: Vec::new(),
        }))
        .expect("inbound");
        let envelope = init
            .outbound
            .recv_timeout(std::time::Duration::from_secs(5))
            .expect("respond");
        let istmo::Frame::Respond {
            result: Ok(bytes), ..
        } = envelope.frame
        else {
            panic!("unexpected {:?}", envelope.frame);
        };
        codec::decode::<String>(&bytes).expect("decode").0
    };
    assert_eq!(served_on(1, "thread"), here);
    assert_ne!(served_on(2, "blocking_thread"), here);
}

#[test]
fn shortcut_opt_outs_keep_every_call_on_the_wire() {
    let rt = Runtime::mock().runtime;
    rt.register_host(WireOnlyHost::new(Impl));
    let client = WireOnlyClient::from_runtime(&rt).expect("client");
    assert!(!client.is_local(), "shortcut = false");
    assert_eq!(pollster::block_on(client.ping()).expect("ping"), 7);

    let rt = Runtime::mock().runtime;
    rt.register_host_wire_only(CameraHost::new(Impl));
    let client = CameraClient::from_runtime(&rt).expect("client");
    assert!(!client.is_local(), "registered wire-only");
    assert_eq!(
        pollster::block_on(client.origin()).expect("origin"),
        "Local"
    );
    // Wire-only also means a blocking worker, inline methods included.
    let here = thread_name(std::thread::current().id());
    assert_ne!(pollster::block_on(client.thread()).expect("thread"), here);
}

#[test]
fn frame_sink_methods_refuse_callers_outside_the_process() {
    let rt = Runtime::mock().runtime;
    let host = Arc::new(CameraHost::new(Impl));
    rt.register_host_arc(Arc::clone(&host));
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let payload = codec::encode(&(sink.id(),)).expect("payload");
    for origin in [CallOrigin::Peer, CallOrigin::Remote] {
        let refused = pollster::block_on(host.dispatch_from(
            origin,
            None,
            "start",
            &payload,
            CancelToken::new(),
        ));
        assert!(
            matches!(refused, Err(DispatchError::OriginRefused { .. })),
            "{origin:?}: {refused:?}"
        );
    }
    let served = pollster::block_on(host.dispatch_from(
        CallOrigin::Local,
        None,
        "start",
        &payload,
        CancelToken::new(),
    ));
    assert!(matches!(served, Ok(Outcome::Ok(_))), "{served:?}");
    let peer = pollster::block_on(host.dispatch_from(
        CallOrigin::Peer,
        None,
        "origin",
        &codec::encode(&()).expect("unit"),
        CancelToken::new(),
    ));
    let Ok(Outcome::Ok(bytes)) = peer else {
        panic!("origin from a peer: {peer:?}");
    };
    assert_eq!(codec::decode::<String>(&bytes).expect("string").0, "Peer");
}

#[test]
fn clients_refuse_to_send_a_sink_to_another_process() {
    let rt = Runtime::mock().runtime;
    rt.declare_remote_plugin("test.ctx.camera");
    let client = CameraClient::from_runtime(&rt).expect("client");
    let sink = rt
        .open_frame_sink::<u32>(SinkPolicy::latest())
        .expect("open");
    let err = pollster::block_on(client.start(sink.id())).expect_err("remote");
    assert!(
        matches!(err, IstmoError::FrameSinkNotShareable("test.ctx.camera")),
        "{err}"
    );
}

#[test]
fn a_detached_context_has_no_runtime() {
    let ctx = HostCtx::detached(CallOrigin::Local);
    assert!(matches!(ctx.runtime(), Err(IstmoError::RuntimeShutDown)));
    assert!(ctx.frame_sender(FrameSinkId::<u32>::from_raw(1)).is_err());
}
