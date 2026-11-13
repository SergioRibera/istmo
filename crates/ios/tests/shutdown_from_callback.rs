//! `istmo_ios_shutdown` called on the pump thread (from inside a
//! callback) must not try to join that same thread. Separate test binary:
//! the transport is a process-wide one-shot.

use std::os::raw::c_void;
use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::time::Duration;

use istmo_core::RuntimeInit;
use istmo_ios::{IstmoIosCallbacks, istmo_ios_shutdown, istmo_ios_start};

#[unsafe(no_mangle)]
pub extern "Rust" fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit {
    init.finish()
}

static SHUTDOWN_RESULT: OnceLock<Sender<bool>> = OnceLock::new();

unsafe extern "C" fn on_call(
    _ctx: *mut c_void,
    _call_id: u64,
    _plugin_id_utf8: *const u8,
    _plugin_id_len: usize,
    _instance_id: u64,
    _method_utf8: *const u8,
    _method_len: usize,
    _payload: *const u8,
    _payload_len: usize,
) {
    let joined = istmo_ios_shutdown();
    if let Some(tx) = SHUTDOWN_RESULT.get() {
        tx.send(joined).unwrap();
    }
}

const unsafe extern "C" fn on_id(_ctx: *mut c_void, _id: u64) {}

const unsafe extern "C" fn on_create_instance(
    _ctx: *mut c_void,
    _call_id: u64,
    _plugin_id_utf8: *const u8,
    _plugin_id_len: usize,
    _payload: *const u8,
    _payload_len: usize,
) {
}

const unsafe extern "C" fn on_respond(
    _ctx: *mut c_void,
    _call_id: u64,
    _ok: bool,
    _payload: *const u8,
    _payload_len: usize,
) {
}

const unsafe extern "C" fn on_event(
    _ctx: *mut c_void,
    _stream_id: u64,
    _payload: *const u8,
    _payload_len: usize,
) {
}

const unsafe extern "C" fn on_stream_end(
    _ctx: *mut c_void,
    _stream_id: u64,
    _reason: u32,
    _payload: *const u8,
    _payload_len: usize,
) {
}

#[allow(clippy::too_many_arguments)]
const unsafe extern "C" fn on_notify(
    _ctx: *mut c_void,
    _plugin_id_utf8: *const u8,
    _plugin_id_len: usize,
    _instance_id: u64,
    _method_utf8: *const u8,
    _method_len: usize,
    _payload: *const u8,
    _payload_len: usize,
) {
}

#[test]
fn shutdown_inside_a_callback_skips_the_self_join() {
    let (tx, rx) = channel();
    SHUTDOWN_RESULT.set(tx).unwrap();
    let callbacks = IstmoIosCallbacks {
        ctx: std::ptr::null_mut(),
        on_call: Some(on_call),
        on_cancel: Some(on_id),
        on_create_instance: Some(on_create_instance),
        on_destroy_instance: Some(on_id),
        on_respond: Some(on_respond),
        on_event: Some(on_event),
        on_stream_end: Some(on_stream_end),
        on_release_native_handle: Some(on_id),
        on_notify: Some(on_notify),
    };
    // SAFETY: the callbacks are thread-safe and never read their buffers.
    assert!(unsafe { istmo_ios_start(callbacks) });

    let runtime = istmo_core::Runtime::global().unwrap();
    let _handle = runtime
        .call("test.ios.remote", None, "noop", Vec::new())
        .unwrap();

    let joined = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("callback ran and shutdown returned instead of deadlocking");
    assert!(!joined, "the pump thread cannot report joining itself");
    assert!(!istmo_ios_shutdown(), "the pump was already stopped");
}
