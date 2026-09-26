#![allow(unreachable_pub)]

use std::os::raw::c_void;
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

pub type OnCancelFn = unsafe extern "C" fn(ctx: *mut c_void, call_id: u64);

pub type OnCreateInstanceFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    payload: *const u8,
    payload_len: usize,
);

pub type OnDestroyInstanceFn = unsafe extern "C" fn(ctx: *mut c_void, instance_id: u64);

pub type OnRespondFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    call_id: u64,
    ok: bool,
    payload: *const u8,
    payload_len: usize,
);

pub type OnEventFn =
    unsafe extern "C" fn(ctx: *mut c_void, stream_id: u64, payload: *const u8, payload_len: usize);

pub type OnStreamEndFn = unsafe extern "C" fn(
    ctx: *mut c_void,
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
);

pub type OnReleaseNativeHandleFn = unsafe extern "C" fn(ctx: *mut c_void, handle_id: u64);

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

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct IstmoIosCallbacks {
    pub ctx: *mut c_void,
    pub on_call: OnCallFn,
    pub on_cancel: OnCancelFn,
    pub on_create_instance: OnCreateInstanceFn,
    pub on_destroy_instance: OnDestroyInstanceFn,
    pub on_respond: OnRespondFn,
    pub on_event: OnEventFn,
    pub on_stream_end: OnStreamEndFn,
    pub on_release_native_handle: OnReleaseNativeHandleFn,
    pub on_notify: OnNotifyFn,
}

// SAFETY: `ctx` is opaque; the Swift side is responsible for keeping the
// backing object alive across the pump thread's lifetime. Function pointers
// are always `Send + Sync`.
unsafe impl Send for IstmoIosCallbacks {}
unsafe impl Sync for IstmoIosCallbacks {}

#[unsafe(no_mangle)]
pub extern "C" fn istmo_ios_start(callbacks: IstmoIosCallbacks) -> bool {
    match start(callbacks) {
        Ok(()) => true,
        Err(err) => {
            tracing::error!(?err, "istmo_ios_start failed");
            false
        }
    }
}

fn start(callbacks: IstmoIosCallbacks) -> Result<(), IosRuntimeError> {
    let init = Runtime::init(RuntimeConfig::inline())?;
    let init = __istmo_configure_runtime(init);
    let handles = pump::spawn(callbacks, init.outbound);
    state::install(RuntimeState {
        pump_shutdown: std::sync::Mutex::new(Some(handles.shutdown)),
        pump_join: std::sync::Mutex::new(Some(handles.join)),
    })?;
    Ok(())
}

#[unsafe(no_mangle)]
pub extern "C" fn istmo_ios_shutdown() {
    if let Err(err) = shutdown() {
        tracing::error!(?err, "istmo_ios_shutdown failed");
    }
}

fn shutdown() -> Result<(), IosRuntimeError> {
    let state = state::get()?;
    if let Ok(runtime) = Runtime::global() {
        runtime.shutdown();
    }
    if let Ok(mut guard) = state.pump_shutdown.lock() {
        guard.take();
    }
    if let Ok(mut guard) = state.pump_join.lock()
        && let Some(handle) = guard.take()
    {
        let _ = handle.join();
    }
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_response(
    call_id: u64,
    ok: bool,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: caller-guaranteed layout per the doc above.
    let bytes = unsafe { copy_bytes(payload, payload_len) };
    let result = if ok { Ok(bytes) } else { Err(bytes) };
    dispatch_inbound(Envelope::new(Frame::Respond {
        call_id: CallId(call_id),
        result,
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_event(
    stream_id: u64,
    payload: *const u8,
    payload_len: usize,
) {
    let bytes = unsafe { copy_bytes(payload, payload_len) };
    dispatch_inbound(Envelope::new(Frame::Event {
        stream_id: StreamId(stream_id),
        payload: bytes,
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_stream_end(
    stream_id: u64,
    reason: u32,
    err_payload: *const u8,
    err_payload_len: usize,
) {
    let reason = match reason {
        0 => StreamEndReason::Complete,
        1 => StreamEndReason::Cancelled,
        2 => StreamEndReason::Error(unsafe { copy_bytes(err_payload, err_payload_len) }),
        other => {
            tracing::error!(other, "istmo_ios_submit_stream_end unknown reason tag");
            return;
        }
    };
    dispatch_inbound(Envelope::new(Frame::StreamEnd {
        stream_id: StreamId(stream_id),
        reason,
    }));
}

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
    // SAFETY: caller-guaranteed layout per each buffer's doc contract.
    let result = unsafe {
        submit_call(
            call_id,
            plugin_id_utf8,
            plugin_id_len,
            instance_id,
            method_utf8,
            method_len,
            payload,
            payload_len,
        )
    };
    if let Err(err) = result {
        tracing::error!(?err, "istmo_ios_submit_call failed");
    }
}

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
    let result = unsafe {
        submit_notify(
            plugin_id_utf8,
            plugin_id_len,
            instance_id,
            method_utf8,
            method_len,
            payload,
            payload_len,
        )
    };
    if let Err(err) = result {
        tracing::error!(?err, "istmo_ios_submit_notify failed");
    }
}

unsafe fn submit_notify(
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) -> Result<(), IosRuntimeError> {
    let plugin_id = unsafe { copy_utf8(plugin_id_utf8, plugin_id_len) }?;
    let method = unsafe { copy_utf8(method_utf8, method_len) }?;
    let payload = unsafe { copy_bytes(payload, payload_len) };
    let instance_id = if instance_id == 0 {
        None
    } else {
        Some(InstanceId(instance_id))
    };
    Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Notify {
        plugin_id,
        instance_id,
        method,
        payload,
    }))?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
unsafe fn submit_call(
    call_id: u64,
    plugin_id_utf8: *const u8,
    plugin_id_len: usize,
    instance_id: u64,
    method_utf8: *const u8,
    method_len: usize,
    payload: *const u8,
    payload_len: usize,
) -> Result<(), IosRuntimeError> {
    let plugin_id = unsafe { copy_utf8(plugin_id_utf8, plugin_id_len) }?;
    let method = unsafe { copy_utf8(method_utf8, method_len) }?;
    let payload = unsafe { copy_bytes(payload, payload_len) };
    let instance_id = if instance_id == 0 {
        None
    } else {
        Some(InstanceId(instance_id))
    };
    Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Call {
        call_id: CallId(call_id),
        plugin_id,
        instance_id,
        method,
        payload,
    }))?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_early_latest(
    channel_utf8: *const u8,
    channel_len: usize,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: caller-guaranteed layout per the parameters' doc contracts.
    let result = unsafe { submit_early_latest(channel_utf8, channel_len, payload, payload_len) };
    if let Err(err) = result {
        tracing::error!(?err, "istmo_ios_submit_early_latest failed");
    }
}

unsafe fn submit_early_latest(
    channel_utf8: *const u8,
    channel_len: usize,
    payload: *const u8,
    payload_len: usize,
) -> Result<(), IosRuntimeError> {
    let channel = unsafe { copy_utf8(channel_utf8, channel_len) }?;
    let bytes = unsafe { copy_bytes(payload, payload_len) };
    Runtime::submit_early_event(channel, EarlyEventKind::Latest, bytes)?;
    Ok(())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_early_queue(
    channel_utf8: *const u8,
    channel_len: usize,
    capacity: u32,
    payload: *const u8,
    payload_len: usize,
) {
    // SAFETY: caller-guaranteed layout per the parameters' doc contracts.
    let result =
        unsafe { submit_early_queue(channel_utf8, channel_len, capacity, payload, payload_len) };
    if let Err(err) = result {
        tracing::error!(?err, "istmo_ios_submit_early_queue failed");
    }
}

unsafe fn submit_early_queue(
    channel_utf8: *const u8,
    channel_len: usize,
    capacity: u32,
    payload: *const u8,
    payload_len: usize,
) -> Result<(), IosRuntimeError> {
    let channel = unsafe { copy_utf8(channel_utf8, channel_len) }?;
    let bytes = unsafe { copy_bytes(payload, payload_len) };
    Runtime::submit_early_event(channel, EarlyEventKind::Queue { capacity }, bytes)?;
    Ok(())
}

fn dispatch_inbound(envelope: Envelope) {
    let runtime = match Runtime::global() {
        Ok(rt) => rt,
        Err(err) => {
            tracing::error!(?err, "inbound frame dropped: runtime not started");
            return;
        }
    };
    if let Err(err) = Arc::clone(&runtime).dispatch_inbound(envelope) {
        tracing::error!(?err, "inbound dispatch failed");
    }
}

unsafe fn copy_bytes(ptr: *const u8, len: usize) -> Vec<u8> {
    if len == 0 || ptr.is_null() {
        return Vec::new();
    }
    // SAFETY: caller-guaranteed layout.
    unsafe { slice::from_raw_parts(ptr, len).to_vec() }
}

unsafe fn copy_utf8(ptr: *const u8, len: usize) -> Result<String, IosRuntimeError> {
    let bytes = unsafe { copy_bytes(ptr, len) };
    String::from_utf8(bytes).map_err(|_| IosRuntimeError::InvalidUtf8)
}
