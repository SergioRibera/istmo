//! `#[unsafe(no_mangle)]` entry points loaded by the JVM via `System.loadLibrary`.
//!
//! Symbol naming follows the JNI convention for the Kotlin class
//! `dev.istmo.runtime.IstmoRuntime`.

#![allow(unreachable_pub)]

use std::sync::Arc;

use istmo_core::{
    CallId, Envelope, Frame, InstanceId, Runtime, RuntimeConfig, RuntimeInit, StreamEndReason,
    StreamId,
};
use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong};

use crate::error::AndroidRuntimeError;
use crate::pump;
use crate::state::{self, RuntimeState};

// The `istmo::runtime!` macro is required to link the demo cdylib: it emits
// `__istmo_configure_runtime` which nativeStart calls once, immediately after
// the process runtime is installed, to declare plugin ids and register host
// dispatchers. Missing symbol == user forgot to call `istmo::runtime!` — the
// linker error is the intended signal.
unsafe extern "Rust" {
    safe fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit;
}

/// Kotlin: `external fun nativeStart(runtimeClass: Class<*>): Boolean`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeStart<'local>(
    env: JNIEnv<'local>,
    _caller: JClass<'local>,
    runtime_class: JClass<'local>,
) -> jboolean {
    match start(&env, runtime_class) {
        Ok(()) => JNI_TRUE,
        Err(err) => {
            tracing::error!(?err, "istmo nativeStart failed");
            JNI_FALSE
        }
    }
}

fn start<'local>(
    env: &JNIEnv<'local>,
    runtime_class: JClass<'local>,
) -> Result<(), AndroidRuntimeError> {
    let jvm = Arc::new(env.get_java_vm()?);
    let class_ref = env.new_global_ref(runtime_class)?;

    let init = Runtime::init(RuntimeConfig::inline())?;
    let init = __istmo_configure_runtime(init);
    let handles = pump::spawn(jvm, class_ref, init.outbound);

    state::install(RuntimeState {
        pump_shutdown: std::sync::Mutex::new(Some(handles.shutdown)),
        pump_join: std::sync::Mutex::new(Some(handles.join)),
    })?;
    Ok(())
}

/// Kotlin: `external fun nativeSubmitResponse(callId: Long, ok: Boolean, payload: ByteArray)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse<'local>(
    env: JNIEnv<'local>,
    _caller: JClass<'local>,
    call_id: jlong,
    ok: jboolean,
    payload: JByteArray<'local>,
) {
    if let Err(err) = submit_response(&env, call_id, ok, &payload) {
        tracing::error!(?err, "istmo nativeSubmitResponse failed");
    }
}

#[allow(clippy::cast_sign_loss)]
fn submit_response<'local>(
    env: &JNIEnv<'local>,
    call_id: jlong,
    ok: jboolean,
    payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let bytes = env.convert_byte_array(payload)?;
    let result = if ok == JNI_TRUE {
        Ok(bytes)
    } else {
        Err(bytes)
    };
    let envelope = Envelope::new(Frame::Respond {
        call_id: CallId(call_id as u64),
        result,
    });
    Runtime::global()?.dispatch_inbound(envelope)?;
    Ok(())
}

/// Kotlin: `external fun nativeSubmitEvent(streamId: Long, payload: ByteArray)`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent<'local>(
    env: JNIEnv<'local>,
    _caller: JClass<'local>,
    stream_id: jlong,
    payload: JByteArray<'local>,
) {
    if let Err(err) = submit_event(&env, stream_id, &payload) {
        tracing::error!(?err, "istmo nativeSubmitEvent failed");
    }
}

#[allow(clippy::cast_sign_loss)]
fn submit_event<'local>(
    env: &JNIEnv<'local>,
    stream_id: jlong,
    payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let bytes = env.convert_byte_array(payload)?;
    let envelope = Envelope::new(Frame::Event {
        stream_id: StreamId(stream_id as u64),
        payload: bytes,
    });
    Runtime::global()?.dispatch_inbound(envelope)?;
    Ok(())
}

/// Kotlin: `external fun nativeSubmitStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray?)`.
///
/// `reason` maps to `StreamEndReason` variants:
/// * `0` — `Complete`
/// * `1` — `Cancelled`
/// * `2` — `Error(errorPayload)` (payload required, otherwise treated as empty).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd<'local>(
    env: JNIEnv<'local>,
    _caller: JClass<'local>,
    stream_id: jlong,
    reason: jint,
    error_payload: JByteArray<'local>,
) {
    if let Err(err) = submit_stream_end(&env, stream_id, reason, &error_payload) {
        tracing::error!(?err, "istmo nativeSubmitStreamEnd failed");
    }
}

#[allow(clippy::cast_sign_loss)]
fn submit_stream_end<'local>(
    env: &JNIEnv<'local>,
    stream_id: jlong,
    reason: jint,
    error_payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let reason = match reason {
        0 => StreamEndReason::Complete,
        1 => StreamEndReason::Cancelled,
        2 => {
            let bytes = if error_payload.is_null() {
                Vec::new()
            } else {
                env.convert_byte_array(error_payload)?
            };
            StreamEndReason::Error(bytes)
        }
        other => {
            tracing::error!(other, "istmo nativeSubmitStreamEnd unknown reason tag");
            return Ok(());
        }
    };
    let envelope = Envelope::new(Frame::StreamEnd {
        stream_id: StreamId(stream_id as u64),
        reason,
    });
    Runtime::global()?.dispatch_inbound(envelope)?;
    Ok(())
}

/// Kotlin: `external fun nativeSubmitCall(callId: Long, pluginId: String, instanceId: Long, method: String, payload: ByteArray)`.
///
/// Symmetric counterpart of the outbound Call pump: Kotlin uses it to invoke
/// a Rust-hosted plugin (a trait declared in the process's `hosts:` section).
/// `instanceId == 0` encodes `None`; any positive value is treated as the
/// wire `InstanceId`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    call_id: jlong,
    plugin_id: JString<'local>,
    instance_id: jlong,
    method: JString<'local>,
    payload: JByteArray<'local>,
) {
    if let Err(err) = submit_call(
        &mut env,
        call_id,
        &plugin_id,
        instance_id,
        &method,
        &payload,
    ) {
        tracing::error!(?err, "istmo nativeSubmitCall failed");
    }
}

#[allow(clippy::cast_sign_loss)]
fn submit_call<'local>(
    env: &mut JNIEnv<'local>,
    call_id: jlong,
    plugin_id: &JString<'local>,
    instance_id: jlong,
    method: &JString<'local>,
    payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let plugin_id: String = env.get_string(plugin_id)?.into();
    let method: String = env.get_string(method)?.into();
    let payload = env.convert_byte_array(payload)?;
    let instance_id = if instance_id == 0 {
        None
    } else {
        Some(InstanceId(instance_id as u64))
    };
    let envelope = Envelope::new(Frame::Call {
        call_id: CallId(call_id as u64),
        plugin_id,
        instance_id,
        method,
        payload,
    });
    Runtime::global()?.dispatch_inbound(envelope)?;
    Ok(())
}

/// Kotlin: `external fun nativeSubmitEarlyLatest(channel: String, payload: ByteArray)`.
///
/// Transitional helper: lifecycle-shaped early events (latest-value semantics)
/// bypass the `Frame::…` protocol and go straight into the runtime's
/// [`istmo_core::early_events::LatestValueSlot`]. The intent is to migrate to
/// a dedicated `Frame::EarlyEvent` variant so every crossing is a frame, but
/// that requires a wire-version bump. Documented in CLAUDE.md follow-ups.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    channel: JString<'local>,
    payload: JByteArray<'local>,
) {
    if let Err(err) = submit_early_latest(&mut env, &channel, &payload) {
        tracing::error!(?err, "istmo nativeSubmitEarlyLatest failed");
    }
}

fn submit_early_latest<'local>(
    env: &mut JNIEnv<'local>,
    channel: &JString<'local>,
    payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let channel: String = env.get_string(channel)?.into();
    let bytes = env.convert_byte_array(payload)?;
    Runtime::global()?.publish_early_latest(&channel, bytes);
    Ok(())
}

/// Kotlin: `external fun nativeSubmitEarlyQueue(channel: String, capacity: Int, payload: ByteArray)`.
///
/// Companion to [`Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest`]
/// for deep-link-shaped events (bounded FIFO buffered until a subscriber
/// attaches). Same transitional rationale.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyQueue<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    channel: JString<'local>,
    capacity: jint,
    payload: JByteArray<'local>,
) {
    if let Err(err) = submit_early_queue(&mut env, &channel, capacity, &payload) {
        tracing::error!(?err, "istmo nativeSubmitEarlyQueue failed");
    }
}

#[allow(clippy::cast_sign_loss)]
fn submit_early_queue<'local>(
    env: &mut JNIEnv<'local>,
    channel: &JString<'local>,
    capacity: jint,
    payload: &JByteArray<'local>,
) -> Result<(), AndroidRuntimeError> {
    let channel: String = env.get_string(channel)?.into();
    let bytes = env.convert_byte_array(payload)?;
    let capacity = if capacity <= 0 { 0 } else { capacity as usize };
    Runtime::global()?.publish_early_queue(&channel, capacity, bytes);
    Ok(())
}

/// Kotlin: `external fun nativeShutdown()`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown(
    _env: JNIEnv<'_>,
    _caller: JClass<'_>,
) {
    if let Err(err) = shutdown() {
        tracing::error!(?err, "istmo nativeShutdown failed");
    }
}

fn shutdown() -> Result<(), AndroidRuntimeError> {
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
