use std::os::raw::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use istmo_core::{
    CancelToken, Dispatch, DispatchError, DispatchFuture, InstanceId, Outcome, Plugin, RuntimeInit,
};
use istmo_ios::{
    IstmoIosCallbacks, istmo_ios_shutdown, istmo_ios_start, istmo_ios_submit_call,
    istmo_ios_submit_notify, istmo_ios_submit_response,
};

const PLUGIN_ID: &str = "test.ios.echo";

struct EchoHost;

impl Plugin for EchoHost {
    const PLUGIN_ID: &'static str = PLUGIN_ID;
}

impl Dispatch for EchoHost {
    fn plugin_id(&self) -> &'static str {
        PLUGIN_ID
    }

    fn dispatch<'a>(
        &'a self,
        _instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            match method {
                "echo" => {
                    let ((msg,), _): ((String,), _) =
                        istmo_core::codec::decode(payload).map_err(DispatchError::Decode)?;
                    let reply = format!("echo:{msg}");
                    let bytes = istmo_core::codec::encode(&reply).map_err(DispatchError::Encode)?;
                    Ok(Outcome::Ok(bytes))
                }
                "ping" => {
                    pings().lock().unwrap().push(payload.to_vec());
                    Ok(Outcome::Ok(Vec::new()))
                }
                other => Err(DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

#[unsafe(no_mangle)]
pub extern "Rust" fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit {
    init.host(EchoHost).finish()
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Received {
    Respond {
        call_id: u64,
        ok: bool,
        payload: Vec<u8>,
    },
    Event {
        stream_id: u64,
        payload: Vec<u8>,
    },
    StreamEnd {
        stream_id: u64,
        reason: u32,
        payload: Vec<u8>,
    },
    Call {
        call_id: u64,
        plugin_id: String,
        instance_id: u64,
        method: String,
        payload: Vec<u8>,
    },
    Cancel {
        call_id: u64,
    },
    CreateInstance {
        call_id: u64,
        plugin_id: String,
        payload: Vec<u8>,
    },
    DestroyInstance {
        instance_id: u64,
    },
    ReleaseNativeHandle {
        handle_id: u64,
    },
    Notify {
        plugin_id: String,
        instance_id: u64,
        method: String,
        payload: Vec<u8>,
    },
}

type ReceivedSink = Arc<Mutex<Vec<Received>>>;

static SINK: OnceLock<ReceivedSink> = OnceLock::new();

fn sink() -> &'static ReceivedSink {
    SINK.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
}

/// Payloads of the `ping` notifications the Rust host received.
fn pings() -> &'static Mutex<Vec<Vec<u8>>> {
    static PINGS: OnceLock<Mutex<Vec<Vec<u8>>>> = OnceLock::new();
    PINGS.get_or_init(|| Mutex::new(Vec::new()))
}

unsafe extern "C" fn on_call(
    _ctx: *mut c_void,
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let plugin_id = unsafe { copy_str(plugin_id_utf8, plugin_id_len) };
    // SAFETY: the pump passes buffers valid for the given lengths.
    let method = unsafe { copy_str(method_utf8, method_len) };
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink().lock().unwrap().push(Received::Call {
        call_id,
        plugin_id,
        instance_id,
        method,
        payload,
    });
}

unsafe extern "C" fn on_cancel(_ctx: *mut c_void, call_id: u64) {
    sink().lock().unwrap().push(Received::Cancel { call_id });
}

unsafe extern "C" fn on_create_instance(
    _ctx: *mut c_void,
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let plugin_id = unsafe { copy_str(plugin_id_utf8, plugin_id_len) };
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink().lock().unwrap().push(Received::CreateInstance {
        call_id,
        plugin_id,
        payload,
    });
}

unsafe extern "C" fn on_destroy_instance(_ctx: *mut c_void, instance_id: u64) {
    sink()
        .lock()
        .unwrap()
        .push(Received::DestroyInstance { instance_id });
}

unsafe extern "C" fn on_respond(
    _ctx: *mut c_void,
    call_id: u64,
    ok: bool,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink().lock().unwrap().push(Received::Respond {
        call_id,
        ok,
        payload,
    });
}

unsafe extern "C" fn on_event(
    _ctx: *mut c_void,
    stream_id: u64,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink()
        .lock()
        .unwrap()
        .push(Received::Event { stream_id, payload });
}

unsafe extern "C" fn on_stream_end(
    _ctx: *mut c_void,
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(err_payload, err_payload_len) };
    sink().lock().unwrap().push(Received::StreamEnd {
        stream_id,
        reason,
        payload,
    });
}

unsafe extern "C" fn on_release_native_handle(_ctx: *mut c_void, handle_id: u64) {
    sink()
        .lock()
        .unwrap()
        .push(Received::ReleaseNativeHandle { handle_id });
}

#[allow(clippy::too_many_arguments)]
unsafe extern "C" fn on_notify(
    _ctx: *mut c_void,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: the pump passes buffers valid for the given lengths.
    let plugin_id = unsafe { copy_str(plugin_id_utf8, plugin_id_len) };
    // SAFETY: the pump passes buffers valid for the given lengths.
    let method = unsafe { copy_str(method_utf8, method_len) };
    // SAFETY: the pump passes buffers valid for the given lengths.
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink().lock().unwrap().push(Received::Notify {
        plugin_id,
        instance_id,
        method,
        payload,
    });
}

unsafe fn copy_str(ptr: *const u8, len: usize) -> String {
    // SAFETY: forwarded from the caller.
    String::from_utf8(unsafe { copy_vec(ptr, len) }).expect("valid utf-8 from pump")
}

unsafe fn copy_vec(ptr: *const u8, len: usize) -> Vec<u8> {
    if len == 0 || ptr.is_null() {
        Vec::new()
    } else {
        // SAFETY: the caller passes `len` readable bytes at `ptr`.
        unsafe { std::slice::from_raw_parts(ptr, len).to_vec() }
    }
}

fn callbacks() -> IstmoIosCallbacks {
    IstmoIosCallbacks {
        ctx: std::ptr::null_mut(),
        on_call: Some(on_call),
        on_cancel: Some(on_cancel),
        on_create_instance: Some(on_create_instance),
        on_destroy_instance: Some(on_destroy_instance),
        on_respond: Some(on_respond),
        on_event: Some(on_event),
        on_stream_end: Some(on_stream_end),
        on_release_native_handle: Some(on_release_native_handle),
        on_notify: Some(on_notify),
    }
}

fn start(callbacks: IstmoIosCallbacks) -> bool {
    // SAFETY: the test callbacks are thread-safe, ignore `ctx` and only
    // read their buffers within the documented lengths.
    unsafe { istmo_ios_start(callbacks) }
}

/// Count of recorded callbacks matching `pred`.
fn received(pred: impl Fn(&Received) -> bool) -> usize {
    sink().lock().unwrap().iter().filter(|r| pred(r)).count()
}

/// Give the pump time to deliver anything that is (wrongly) in flight.
fn settle() {
    std::thread::sleep(Duration::from_millis(100));
}

fn wait_for<T>(mut check: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if let Some(v) = check() {
            return v;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("timeout waiting for callback");
}

// The transport is a process-wide one-shot (start once, shut down once),
// so the whole lifecycle runs as one ordered test.
#[test]
#[expect(
    clippy::too_many_lines,
    reason = "one ordered scenario over a one-shot transport"
)]
fn ios_transport_end_to_end() {
    let mut missing_notify = callbacks();
    missing_notify.on_notify = None;
    assert!(!start(missing_notify), "a null callback must be rejected");
    assert!(
        istmo_core::Runtime::global().is_err(),
        "a rejected start must not install the runtime"
    );

    assert!(start(callbacks()), "start should succeed");
    assert!(!start(callbacks()), "a second start must fail");

    let call_id: u64 = 42;
    let args = istmo_core::codec::encode(&(String::from("hello"),)).unwrap();
    // SAFETY: every pointer/length pair borrows a live buffer.
    unsafe {
        istmo_ios_submit_call(
            call_id,
            PLUGIN_ID.as_ptr(),
            PLUGIN_ID.len(),
            0,
            "echo".as_ptr(),
            "echo".len(),
            args.as_ptr(),
            args.len(),
        );
    }
    let respond = wait_for(|| {
        sink()
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|r| matches!(r, Received::Respond { call_id: c, .. } if *c == call_id))
            .cloned()
    });
    let Received::Respond {
        ok,
        payload,
        call_id: cid,
    } = respond
    else {
        panic!("expected Respond, got {respond:?}")
    };
    assert_eq!(cid, call_id);
    assert!(ok);
    let (reply, _): (String, _) = istmo_core::codec::decode(&payload).unwrap();
    assert_eq!(reply, "echo:hello");

    let runtime = istmo_core::Runtime::global().expect("started");
    let handle = runtime
        .call("test.ios.remote", None, "noop", Vec::new())
        .expect("call");
    let remote_call_id = handle.call_id().get();

    wait_for(|| {
        sink().lock().unwrap().iter().rev().find_map(|r| match r {
            Received::Call {
                call_id: c,
                plugin_id,
                method,
                ..
            } if *c == remote_call_id && plugin_id == "test.ios.remote" && method == "noop" => {
                Some(())
            }
            _ => None,
        })
    });

    let reply_bytes = istmo_core::codec::encode(&String::from("pong")).unwrap();
    // SAFETY: the payload pointer borrows a live buffer.
    unsafe {
        istmo_ios_submit_response(
            remote_call_id,
            true,
            reply_bytes.as_ptr(),
            reply_bytes.len(),
        );
    }
    let result = handle.recv_blocking().expect("handle recv");
    let bytes = result.expect("ok");
    let (msg, _): (String, _) = istmo_core::codec::decode(&bytes).unwrap();
    assert_eq!(msg, "pong");

    // Swift -> Rust notify reaches the host and produces no response.
    let responds_before = received(|r| matches!(r, Received::Respond { .. }));
    // SAFETY: every pointer/length pair borrows a live buffer.
    unsafe {
        istmo_ios_submit_notify(
            PLUGIN_ID.as_ptr(),
            PLUGIN_ID.len(),
            0,
            "ping".as_ptr(),
            "ping".len(),
            [7u8, 8].as_ptr(),
            2,
        );
    }
    wait_for(|| pings().lock().unwrap().contains(&vec![7, 8]).then_some(()));
    settle();
    assert_eq!(
        received(|r| matches!(r, Received::Respond { .. })),
        responds_before,
        "a notify must not be answered"
    );

    // Rust -> Swift notify lands on `on_notify`.
    runtime
        .notify(
            "test.ios.remote",
            Some(InstanceId(9)),
            "tick",
            vec![1, 2, 3],
        )
        .expect("notify");
    wait_for(|| {
        (received(|r| {
            matches!(
                r,
                Received::Notify { plugin_id, instance_id: 9, method, payload }
                    if plugin_id == "test.ios.remote" && method == "tick" && payload == &[1, 2, 3]
            )
        }) == 1)
            .then_some(())
    });

    // Invalid UTF-8 is rejected before reaching the runtime.
    let bad_utf8 = [0xff_u8, 0xfe];
    let bad_call_id: u64 = 77;
    // SAFETY: every pointer/length pair borrows a live buffer.
    unsafe {
        istmo_ios_submit_call(
            bad_call_id,
            bad_utf8.as_ptr(),
            bad_utf8.len(),
            0,
            "echo".as_ptr(),
            "echo".len(),
            args.as_ptr(),
            args.len(),
        );
        istmo_ios_submit_notify(
            PLUGIN_ID.as_ptr(),
            PLUGIN_ID.len(),
            0,
            bad_utf8.as_ptr(),
            bad_utf8.len(),
            std::ptr::null(),
            0,
        );
        // Null pointer with a non-zero length is refused, not dereferenced.
        istmo_ios_submit_call(
            bad_call_id,
            std::ptr::null(),
            4,
            0,
            "echo".as_ptr(),
            "echo".len(),
            args.as_ptr(),
            args.len(),
        );
    }
    settle();
    assert_eq!(
        received(|r| matches!(r, Received::Respond { call_id, .. } if *call_id == bad_call_id)),
        0
    );
    assert_eq!(pings().lock().unwrap().len(), 1);

    assert!(istmo_ios_shutdown(), "shutdown should join the pump");
    assert!(
        !istmo_ios_shutdown(),
        "a second shutdown has nothing to join"
    );

    // Submissions after shutdown are dropped without crashing and
    // without any callback firing.
    let total_before = sink().lock().unwrap().len();
    // SAFETY: every pointer/length pair borrows a live buffer.
    unsafe {
        istmo_ios_submit_call(
            100,
            PLUGIN_ID.as_ptr(),
            PLUGIN_ID.len(),
            0,
            "echo".as_ptr(),
            "echo".len(),
            args.as_ptr(),
            args.len(),
        );
        istmo_ios_submit_response(remote_call_id, true, std::ptr::null(), 0);
        istmo_ios_submit_notify(
            PLUGIN_ID.as_ptr(),
            PLUGIN_ID.len(),
            0,
            "ping".as_ptr(),
            "ping".len(),
            std::ptr::null(),
            0,
        );
    }
    settle();
    assert_eq!(sink().lock().unwrap().len(), total_before);
    assert!(!start(callbacks()), "the transport cannot restart");
}
