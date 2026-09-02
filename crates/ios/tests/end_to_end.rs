//! End-to-end verification of the `@_cdecl` transport.
//!
//! Swift is simulated by:
//!
//! 1. Registering a callback table whose function pointers push received
//!    frames into a shared `Mutex<Vec<Received>>`.
//! 2. Calling `istmo_ios_submit_call` with a bincode-encoded argument tuple.
//! 3. Waiting for the `on_respond` callback to fire.
//!
//! The hosted plugin (`Echo`) lives in this crate; `__istmo_configure_runtime`
//! is provided directly, standing in for what the `istmo::runtime!` macro
//! would emit in a real cdylib.

use std::os::raw::c_void;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use bincode::config::Configuration;
use istmo_core::{
    Dispatch, DispatchError, DispatchFuture, InstanceId, Outcome, Plugin, RuntimeInit,
};
use istmo_ios::{
    IstmoIosCallbacks, istmo_ios_shutdown, istmo_ios_start, istmo_ios_submit_call,
    istmo_ios_submit_response,
};

const CODEC: Configuration = bincode::config::standard();
const PLUGIN_ID: &str = "test.ios.echo";

// -- Hosted plugin ----------------------------------------------------------

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
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            match method {
                "echo" => {
                    let ((msg,), _): ((String,), _) =
                        bincode::decode_from_slice(payload, CODEC)
                            .map_err(|e| DispatchError::Decode(istmo_core::CodecError::from(e)))?;
                    let reply = format!("echo:{msg}");
                    let bytes = bincode::encode_to_vec(&reply, CODEC)
                        .map_err(|e| DispatchError::Encode(istmo_core::CodecError::from(e)))?;
                    Ok(Outcome::Ok(bytes))
                }
                other => Err(DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

// -- Runtime configuration hook --------------------------------------------

#[unsafe(no_mangle)]
pub extern "Rust" fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit {
    init.host(EchoHost).finish()
}

// -- Callback capture -------------------------------------------------------

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
}

type ReceivedSink = Arc<Mutex<Vec<Received>>>;

/// Global sink pinned for the callback pointers. Test process is single-shot
/// so a `OnceLock` is fine.
static SINK: OnceLock<ReceivedSink> = OnceLock::new();

fn sink() -> &'static ReceivedSink {
    SINK.get_or_init(|| Arc::new(Mutex::new(Vec::new())))
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
    let plugin_id = unsafe { copy_str(plugin_id_utf8, plugin_id_len) };
    let method = unsafe { copy_str(method_utf8, method_len) };
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
    let plugin_id = unsafe { copy_str(plugin_id_utf8, plugin_id_len) };
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
    let payload = unsafe { copy_vec(payload, payload_len) };
    sink().lock().unwrap().push(Received::Event {
        stream_id,
        payload,
    });
}

unsafe extern "C" fn on_stream_end(
    _ctx: *mut c_void,
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
) {
    let payload = unsafe { copy_vec(err_payload, err_payload_len) };
    sink().lock().unwrap().push(Received::StreamEnd {
        stream_id,
        reason,
        payload,
    });
}

unsafe fn copy_str(ptr: *const u8, len: usize) -> String {
    String::from_utf8(unsafe { copy_vec(ptr, len) }).expect("valid utf-8 from pump")
}

unsafe fn copy_vec(ptr: *const u8, len: usize) -> Vec<u8> {
    if len == 0 || ptr.is_null() {
        Vec::new()
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len).to_vec() }
    }
}

fn callbacks() -> IstmoIosCallbacks {
    IstmoIosCallbacks {
        ctx: std::ptr::null_mut(),
        on_call,
        on_cancel,
        on_create_instance,
        on_destroy_instance,
        on_respond,
        on_event,
        on_stream_end,
    }
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

// -- The single end-to-end test --------------------------------------------
//
// Cargo integration tests each get their own process, so `Runtime::init`
// (invoked inside `istmo_ios_start`) firing at most once per file is fine.
// Sub-tests share the same started transport.

#[test]
fn ios_transport_end_to_end() {
    assert!(istmo_ios_start(callbacks()), "start should succeed");

    // --- Hosted call round-trip -------------------------------------------
    let call_id: u64 = 42;
    let args = bincode::encode_to_vec(&(String::from("hello"),), CODEC).unwrap();
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
    let (reply, _): (String, _) = bincode::decode_from_slice(&payload, CODEC).unwrap();
    assert_eq!(reply, "echo:hello");

    // --- Inbound Respond delivery to a Rust-side pending call -------------
    // Simulate a Rust-initiated call whose Respond arrives from Swift.
    let runtime = istmo_core::Runtime::global().expect("started");
    let handle = runtime
        .call("test.ios.remote", None, "noop", Vec::new())
        .expect("call");
    let remote_call_id = handle.call_id().get();
    // Drain the outbound Call frame (Swift would receive it via on_call).
    wait_for(|| {
        sink().lock().unwrap().iter().rev().find_map(|r| match r {
            Received::Call {
                call_id: c,
                plugin_id,
                method,
                ..
            } if *c == remote_call_id
                && plugin_id == "test.ios.remote"
                && method == "noop" =>
            {
                Some(())
            }
            _ => None,
        })
    });
    // Now push the Respond back through the inbound path.
    let reply_bytes = bincode::encode_to_vec(String::from("pong"), CODEC).unwrap();
    unsafe {
        istmo_ios_submit_response(remote_call_id, true, reply_bytes.as_ptr(), reply_bytes.len());
    }
    let result = handle.recv_blocking().expect("handle recv");
    let bytes = result.expect("ok");
    let (msg, _): (String, _) = bincode::decode_from_slice(&bytes, CODEC).unwrap();
    assert_eq!(msg, "pong");

    istmo_ios_shutdown();
}
