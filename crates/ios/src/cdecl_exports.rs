//! `extern "C"` surface linked by the Swift runtime.
//!
//! The C declarations live in `runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h`;
//! `tests/header_abi.rs` keeps the two in sync.
//!
//! Buffer contract shared by every `(ptr, len)` pair below: `ptr` points
//! at `len` initialised, readable bytes that stay valid and unmodified
//! for the duration of the call, or `len == 0` (then `ptr` is ignored and
//! may be null). Strings are UTF-8 without a terminator. Rust copies
//! every buffer before returning.
//!
//! No panic unwinds out of these functions: each body runs under
//! [`ffi_guard`], which logs the panic and returns the failure value.

use std::os::raw::c_void;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::slice;
use std::sync::Arc;

use istmo_core::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, Runtime, RuntimeConfig, RuntimeInit,
    StreamEndReason, StreamId,
};

use crate::error::IosRuntimeError;
use crate::pump;
use crate::state::{self, RuntimeState};

unsafe extern "Rust" {
    safe fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit;
}

/// Outbound `Frame::Call`. Instance id 0 = stateless.
pub type OnCallFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
);

/// Outbound `Frame::Cancel`.
pub type OnCancelFn = unsafe extern "C" fn(ctx: *mut c_void, call_id: u64);

/// Outbound `Frame::CreateInstance`.
pub type OnCreateInstanceFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    payload: *const u8,
    payload_len: usize,
);

/// Outbound `Frame::DestroyInstance`.
pub type OnDestroyInstanceFn = unsafe extern "C" fn(ctx: *mut c_void, instance_id: u64);

/// Outbound `Frame::Respond`.
pub type OnRespondFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    call_id: u64,
    ok: bool,
    payload: *const u8,
    payload_len: usize,
);

/// Outbound `Frame::Event`.
pub type OnEventFn =
    unsafe extern "C" fn(ctx: *mut c_void, stream_id: u64, payload: *const u8, payload_len: usize);

/// Outbound `Frame::StreamEnd`. `reason`: 0 complete, 1 cancelled,
/// 2 error (payload = error bytes).
pub type OnStreamEndFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
);

/// Outbound `Frame::ReleaseNativeHandle`.
pub type OnReleaseNativeHandleFn = unsafe extern "C" fn(ctx: *mut c_void, handle_id: u64);

/// Outbound `Frame::Notify`. Instance id 0 = stateless.
pub type OnNotifyFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
);

/// Callback table passed by value to [`istmo_ios_start`].
///
/// Mirrors `IstmoIosCallbacks` in `istmo_ffi.h`. The function pointers
/// are `Option` because C may hand over NULL; [`istmo_ios_start`]
/// rejects a table with any `None`. Every callback runs on the pump
/// thread, borrows its buffers for the duration of the call only, and
/// receives `ctx` verbatim.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IstmoIosCallbacks {
    pub ctx: *mut c_void,
    pub on_call: Option<OnCallFn>,
    pub on_cancel: Option<OnCancelFn>,
    pub on_create_instance: Option<OnCreateInstanceFn>,
    pub on_destroy_instance: Option<OnDestroyInstanceFn>,
    pub on_respond: Option<OnRespondFn>,
    pub on_event: Option<OnEventFn>,
    pub on_stream_end: Option<OnStreamEndFn>,
    pub on_release_native_handle: Option<OnReleaseNativeHandleFn>,
    pub on_notify: Option<OnNotifyFn>,
}

/// [`IstmoIosCallbacks`] with every function pointer checked present.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Callbacks {
    pub(crate) ctx: *mut c_void,
    pub(crate) on_call: OnCallFn,
    pub(crate) on_cancel: OnCancelFn,
    pub(crate) on_create_instance: OnCreateInstanceFn,
    pub(crate) on_destroy_instance: OnDestroyInstanceFn,
    pub(crate) on_respond: OnRespondFn,
    pub(crate) on_event: OnEventFn,
    pub(crate) on_stream_end: OnStreamEndFn,
    pub(crate) on_release_native_handle: OnReleaseNativeHandleFn,
    pub(crate) on_notify: OnNotifyFn,
}

// SAFETY: `ctx` is an opaque token Rust never dereferences; it is only
// passed back to the callbacks, and the `istmo_ios_start` contract makes
// the Swift side keep it valid for (and usable from) the pump thread
// until `istmo_ios_shutdown` joins it. Function pointers are `Send`.
unsafe impl Send for Callbacks {}

impl TryFrom<IstmoIosCallbacks> for Callbacks {
    type Error = IosRuntimeError;

    fn try_from(table: IstmoIosCallbacks) -> Result<Self, Self::Error> {
        fn required<F>(f: Option<F>, name: &'static str) -> Result<F, IosRuntimeError> {
            f.ok_or(IosRuntimeError::NullCallback(name))
        }
        Ok(Self {
            ctx: table.ctx,
            on_call: required(table.on_call, "on_call")?,
            on_cancel: required(table.on_cancel, "on_cancel")?,
            on_create_instance: required(table.on_create_instance, "on_create_instance")?,
            on_destroy_instance: required(table.on_destroy_instance, "on_destroy_instance")?,
            on_respond: required(table.on_respond, "on_respond")?,
            on_event: required(table.on_event, "on_event")?,
            on_stream_end: required(table.on_stream_end, "on_stream_end")?,
            on_release_native_handle: required(
                table.on_release_native_handle,
                "on_release_native_handle",
            )?,
            on_notify: required(table.on_notify, "on_notify")?,
        })
    }
}

/// Start the runtime and spawn the pump thread.
///
/// Returns `false` (and logs why) when a callback is null, the transport
/// was already started in this process, or startup fails. A started
/// transport cannot be restarted after [`istmo_ios_shutdown`].
///
/// # Safety
///
/// Every function pointer in `callbacks` must be callable from any
/// thread with the argument contract documented on its type, and `ctx`
/// must stay valid for those callbacks until [`istmo_ios_shutdown`]
/// returns `true`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_start(callbacks: IstmoIosCallbacks) -> bool {
    ffi_guard("istmo_ios_start", false, || match start(callbacks) {
        Ok(()) => true,
        Err(err) => {
            tracing::error!(%err, "istmo_ios_start failed");
            false
        }
    })
}

fn start(callbacks: IstmoIosCallbacks) -> Result<(), IosRuntimeError> {
    if state::get().is_ok() {
        return Err(IosRuntimeError::AlreadyStarted);
    }
    let callbacks = Callbacks::try_from(callbacks)?;
    let init = Runtime::init(RuntimeConfig::inline())?;
    let init = __istmo_configure_runtime(init);
    crate::assets::install();
    crate::path::install_from_bundle();
    let handles = pump::spawn(callbacks, init.outbound)?;
    state::install(RuntimeState::new(handles))
}

/// Stop the runtime and the pump thread.
///
/// Returns `true` when this call joined the pump thread: no callback is
/// running or will run again, so `ctx` may be released. Returns `false`
/// when the transport is not running (never started, or already shut
/// down) and when called from inside a callback — the pump thread cannot
/// join itself, so it exits once that callback returns.
#[unsafe(no_mangle)]
pub extern "C" fn istmo_ios_shutdown() -> bool {
    ffi_guard("istmo_ios_shutdown", false, || match shutdown() {
        Ok(joined) => joined,
        Err(err) => {
            tracing::error!(%err, "istmo_ios_shutdown failed");
            false
        }
    })
}

fn shutdown() -> Result<bool, IosRuntimeError> {
    let state = state::get()?;
    if let Ok(runtime) = Runtime::global() {
        runtime.shutdown();
    }
    Ok(state.stop_pump())
}

/// Deliver the response for a call Rust sent through `on_call` or
/// `on_create_instance`. `ok == false` carries the error payload.
///
/// # Safety
///
/// `(payload, payload_len)` follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_response(
    call_id: u64,
    ok: bool,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_response", (), || {
        // SAFETY: forwarded from this function's contract.
        let bytes = match unsafe { copy_bytes(payload, payload_len) } {
            Ok(bytes) => bytes,
            Err(err) => return tracing::error!(%err, "istmo_ios_submit_response dropped"),
        };
        let result = if ok { Ok(bytes) } else { Err(bytes) };
        dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: CallId(call_id),
            result,
        }));
    });
}

/// Push one item onto a stream Rust opened through `on_call`.
///
/// # Safety
///
/// `(payload, payload_len)` follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_event(
    stream_id: u64,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_event", (), || {
        // SAFETY: forwarded from this function's contract.
        let bytes = match unsafe { copy_bytes(payload, payload_len) } {
            Ok(bytes) => bytes,
            Err(err) => return tracing::error!(%err, "istmo_ios_submit_event dropped"),
        };
        dispatch_inbound(Envelope::new(Frame::Event {
            stream_id: StreamId(stream_id),
            payload: bytes,
        }));
    });
}

/// Close a stream. `reason`: 0 complete, 1 cancelled, 2 error with
/// `err_payload`; other tags are logged and dropped.
///
/// # Safety
///
/// `(err_payload, err_payload_len)` follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_stream_end(
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_stream_end", (), || {
        let reason = match reason {
            0 => StreamEndReason::Complete,
            1 => StreamEndReason::Cancelled,
            // SAFETY: forwarded from this function's contract.
            2 => match unsafe { copy_bytes(err_payload, err_payload_len) } {
                Ok(bytes) => StreamEndReason::Error(bytes),
                Err(err) => return tracing::error!(%err, "istmo_ios_submit_stream_end dropped"),
            },
            other => {
                return tracing::error!(other, "istmo_ios_submit_stream_end unknown reason tag");
            }
        };
        dispatch_inbound(Envelope::new(Frame::StreamEnd {
            stream_id: StreamId(stream_id),
            reason,
        }));
    });
}

/// Call a Rust-hosted plugin; the response arrives through `on_respond`
/// (or a stream through `on_event` / `on_stream_end`) under `call_id`.
/// Instance id 0 = stateless.
///
/// # Safety
///
/// Each `(ptr, len)` pair follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_call(
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_call", (), || {
        // SAFETY: forwarded from this function's contract.
        let target = unsafe {
            Target::copy(
                plugin_id_utf8,
                plugin_id_len,
                instance_id,
                method_utf8,
                method_len,
                payload,
                payload_len,
            )
        };
        let result = target.and_then(|t| {
            Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Call {
                call_id: CallId(call_id),
                plugin_id: t.plugin_id,
                instance_id: t.instance_id,
                method: t.method,
                payload: t.payload,
            }))?;
            Ok(())
        });
        if let Err(err) = result {
            tracing::error!(%err, "istmo_ios_submit_call failed");
        }
    });
}

/// Fire-and-forget call to a Rust-hosted plugin; no response follows.
/// Instance id 0 = stateless.
///
/// # Safety
///
/// Each `(ptr, len)` pair follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_notify(
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_notify", (), || {
        // SAFETY: forwarded from this function's contract.
        let target = unsafe {
            Target::copy(
                plugin_id_utf8,
                plugin_id_len,
                instance_id,
                method_utf8,
                method_len,
                payload,
                payload_len,
            )
        };
        let result = target.and_then(|t| {
            Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Notify {
                plugin_id: t.plugin_id,
                instance_id: t.instance_id,
                method: t.method,
                payload: t.payload,
            }))?;
            Ok(())
        });
        if let Err(err) = result {
            tracing::error!(%err, "istmo_ios_submit_notify failed");
        }
    });
}

/// Publish onto a latest-value early-event channel. Buffered when the
/// runtime is not started yet.
///
/// # Safety
///
/// Each `(ptr, len)` pair follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_early_latest(
    channel_utf8: *const u8,
    channel_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_early_latest", (), || {
        // SAFETY: forwarded from this function's contract.
        let result = unsafe {
            submit_early(
                channel_utf8,
                channel_len,
                EarlyEventKind::Latest,
                payload,
                payload_len,
            )
        };
        if let Err(err) = result {
            tracing::error!(%err, "istmo_ios_submit_early_latest failed");
        }
    });
}

/// Publish onto a bounded-queue early-event channel. Buffered when the
/// runtime is not started yet.
///
/// # Safety
///
/// Each `(ptr, len)` pair follows the module's buffer contract.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_early_queue(
    channel_utf8: *const u8,
    channel_len: usize,
    capacity: u32,
    payload: *const u8,
    payload_len: usize,
) {
    ffi_guard("istmo_ios_submit_early_queue", (), || {
        // SAFETY: forwarded from this function's contract.
        let result = unsafe {
            submit_early(
                channel_utf8,
                channel_len,
                EarlyEventKind::Queue { capacity },
                payload,
                payload_len,
            )
        };
        if let Err(err) = result {
            tracing::error!(%err, "istmo_ios_submit_early_queue failed");
        }
    });
}

/// Run `body`, turning a panic into `on_panic`: unwinding into Swift is
/// undefined behaviour.
fn ffi_guard<T>(name: &'static str, on_panic: T, body: impl FnOnce() -> T) -> T {
    // `AssertUnwindSafe`: after a panic the only state touched again is
    // the runtime's, whose mutexes recover from poisoning by design.
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        tracing::error!(name, "panic caught at the iOS FFI boundary");
        on_panic
    })
}

/// Owned copy of the plugin / method / payload triple shared by calls
/// and notifications.
struct Target {
    plugin_id: String,
    instance_id: Option<InstanceId>,
    method: String,
    payload: Vec<u8>,
}

impl Target {
    /// # Safety
    ///
    /// Each `(ptr, len)` pair follows the module's buffer contract.
    unsafe fn copy(
        plugin_id_utf8: *const u8,
        plugin_id_len: usize,
        instance_id: u64,
        method_utf8: *const u8,
        method_len: usize,
        payload: *const u8,
        payload_len: usize,
    ) -> Result<Self, IosRuntimeError> {
        // SAFETY: the three pairs are forwarded from this function's contract.
        unsafe {
            Ok(Self {
                plugin_id: copy_utf8(plugin_id_utf8, plugin_id_len)?,
                instance_id: (instance_id != 0).then_some(InstanceId(instance_id)),
                method: copy_utf8(method_utf8, method_len)?,
                payload: copy_bytes(payload, payload_len)?,
            })
        }
    }
}

/// # Safety
///
/// Each `(ptr, len)` pair follows the module's buffer contract.
unsafe fn submit_early(
    channel_utf8: *const u8,
    channel_len: usize,
    kind: EarlyEventKind,
    payload: *const u8,
    payload_len: usize,
) -> Result<(), IosRuntimeError> {
    // SAFETY: forwarded from this function's contract.
    let channel = unsafe { copy_utf8(channel_utf8, channel_len) }?;
    // SAFETY: forwarded from this function's contract.
    let bytes = unsafe { copy_bytes(payload, payload_len) }?;
    Runtime::submit_early_event(channel, kind, bytes)?;
    Ok(())
}

fn dispatch_inbound(envelope: Envelope) {
    let runtime = match Runtime::global() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!(%err, "inbound frame dropped: runtime not started");
            return;
        }
    };
    if let Err(err) = Arc::clone(&runtime).dispatch_inbound(envelope) {
        tracing::error!(%err, "inbound dispatch failed");
    }
}

/// # Safety
///
/// `(ptr, len)` follows the module's buffer contract.
unsafe fn copy_bytes(ptr: *const u8, len: usize) -> Result<Vec<u8>, IosRuntimeError> {
    if len == 0 {
        return Ok(Vec::new());
    }
    // `slice::from_raw_parts` requires a non-null pointer and a total
    // size of at most `isize::MAX` bytes; both come from foreign code, so
    // check them instead of trusting them.
    if ptr.is_null() || isize::try_from(len).is_err() {
        return Err(IosRuntimeError::InvalidBuffer { len });
    }
    // SAFETY: `ptr` is non-null and `len <= isize::MAX` (checked above);
    // the caller guarantees `len` initialised bytes readable for the
    // duration of this call. `u8` has alignment 1.
    Ok(unsafe { slice::from_raw_parts(ptr, len) }.to_vec())
}

/// # Safety
///
/// `(ptr, len)` follows the module's buffer contract.
unsafe fn copy_utf8(ptr: *const u8, len: usize) -> Result<String, IosRuntimeError> {
    // SAFETY: forwarded from this function's contract.
    let bytes = unsafe { copy_bytes(ptr, len) }?;
    String::from_utf8(bytes).map_err(|_| IosRuntimeError::InvalidUtf8)
}
