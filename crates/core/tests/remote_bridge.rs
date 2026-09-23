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

            let mut out = Vec::new();
            out.extend_from_slice(m.as_bytes());
            out.push(b':');
            out.extend_from_slice(&p);
            Ok(Outcome::Ok(out))
        })
    }
}

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

#[test]
fn app_to_remote_call_and_response_flow_over_bridge_bytes() {
    let app_init = Runtime::mock();
    let app = app_init.runtime;
    let app_outbound = app_init.outbound;
    app.declare_plugin(RemoteHost::PLUGIN_ID);
    app.register_host(AppHost);

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

    let handle = remote
        .call(
            AppHost::PLUGIN_ID,
            None,
            "notify",
            codec::encode(&()).unwrap(),
        )
        .expect("open reverse call");
    let response = pollster::block_on(handle).expect("remote-side call handle");
    let bytes = match response {
        Ok(b) => b,
        Err(err) => panic!("reverse expected Ok, got Err({} bytes)", err.len()),
    };
    let (msg, _) = codec::decode::<String>(&bytes).expect("decode noted");
    assert_eq!(msg, "noted");

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

#[test]
fn declare_remote_plugin_routes_matching_outbound_through_sink() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let typed_outbound = init.outbound;

    let remote_id = "test.remote.compute";
    let local_id = "test.local.echo";
    rt.declare_remote_plugin(remote_id);

    let sink_bytes: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_bytes_cb = Arc::clone(&sink_bytes);
    rt.install_remote_envelope_sink(Arc::new(move |bytes: Vec<u8>| {
        sink_bytes_cb.lock().unwrap().push(bytes);
    }));

    let handle = rt
        .call(remote_id, None, "compute", codec::encode(&()).unwrap())
        .expect("open call");

    assert!(
        typed_outbound.try_recv().is_err(),
        "remote call must not appear on typed outbound",
    );

    let sink_snapshot = sink_bytes.lock().unwrap().clone();
    assert_eq!(sink_snapshot.len(), 1, "sink got one envelope");
    let sink_env = Envelope::from_wire_bytes(&sink_snapshot[0]).expect("decode sink envelope");
    match &sink_env.frame {
        Frame::Call {
            plugin_id, method, ..
        } => {
            assert_eq!(plugin_id, remote_id);
            assert_eq!(method, "compute");
        }
        other => panic!("expected Call in sink, got {other:?}"),
    }

    drop(handle);

    for _ in 0..100 {
        if sink_bytes.lock().unwrap().len() >= 2 {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    let sink_snapshot = sink_bytes.lock().unwrap().clone();
    assert_eq!(sink_snapshot.len(), 2, "cancel should reach the sink");
    let cancel_env = Envelope::from_wire_bytes(&sink_snapshot[1]).expect("decode cancel");
    assert!(matches!(cancel_env.frame, Frame::Cancel { .. }));
    assert!(
        typed_outbound.try_recv().is_err(),
        "cancel must not reach typed outbound either",
    );

    let _handle = rt
        .call(local_id, None, "echo", codec::encode(&()).unwrap())
        .expect("open local call");
    let env = typed_outbound
        .try_recv()
        .expect("local call on typed outbound");
    match env.frame {
        Frame::Call { plugin_id, .. } => assert_eq!(plugin_id, local_id),
        other => panic!("expected Call for local plugin, got {other:?}"),
    }

    assert_eq!(sink_bytes.lock().unwrap().len(), 2);
}

#[test]
fn mark_call_remote_routes_hosted_respond_through_sink() {
    let init = Runtime::mock();
    let rt = init.runtime.clone();
    let typed_outbound = init.outbound;
    rt.register_host(RemoteHost::new());

    let sink_bytes: Arc<Mutex<Vec<Vec<u8>>>> = Arc::new(Mutex::new(Vec::new()));
    let sink_bytes_cb = Arc::clone(&sink_bytes);
    rt.install_remote_envelope_sink(Arc::new(move |bytes: Vec<u8>| {
        sink_bytes_cb.lock().unwrap().push(bytes);
    }));

    let call_id = istmo_core::CallId(4242);
    rt.mark_call_remote(call_id);
    let payload = codec::encode(&()).unwrap();
    let inbound = Envelope::new(Frame::Call {
        call_id,
        plugin_id: RemoteHost::PLUGIN_ID.to_owned(),
        instance_id: None,
        method: "hello".to_owned(),
        payload,
    });
    rt.dispatch_inbound(inbound).expect("inbound accepted");

    for _ in 0..500 {
        if !sink_bytes.lock().unwrap().is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(2));
    }
    let sink_snapshot = sink_bytes.lock().unwrap().clone();
    assert_eq!(sink_snapshot.len(), 1, "response should reach the sink");
    let respond_env = Envelope::from_wire_bytes(&sink_snapshot[0]).expect("decode respond");
    match respond_env.frame {
        Frame::Respond {
            call_id: got_id,
            result,
        } => {
            assert_eq!(got_id, call_id);
            let bytes = result.expect("Ok response");
            let echoed = String::from_utf8(bytes).expect("utf-8 echo");
            assert!(echoed.starts_with("hello:"), "got {echoed:?}");
        }
        other => panic!("expected Respond, got {other:?}"),
    }
    assert!(
        typed_outbound.try_recv().is_err(),
        "respond must not reach typed outbound",
    );
}
