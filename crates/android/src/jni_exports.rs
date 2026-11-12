//! `extern "system"` entry points bound by `dev.istmo.runtime.IstmoRuntime`.
//!
//! Every export runs its body under [`guarded`]: a panic is caught (an
//! unwind out of an `extern "system"` fn aborts the process) and turned
//! into a Java `IllegalStateException`, and so is every failure that
//! points at a caller bug (runtime not started, malformed bytes, bad
//! arguments). Expected races such as a response arriving after its call
//! was cancelled are only logged — see
//! [`AndroidRuntimeError::is_benign_race`].
//!
//! The runtime is process-scoped: `nativeStart` is idempotent while the
//! runtime runs, and after `nativeShutdown` it cannot be started again
//! in the same process (the core runtime sits in a `OnceLock`).

#![allow(
    unreachable_pub,
    reason = "exports are re-exported from the crate root so the symbols survive linking"
)]

use std::any::Any;
use std::panic::{self, AssertUnwindSafe};
use std::sync::Mutex;

use istmo_core::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, Runtime, RuntimeConfig, RuntimeInit,
    StreamEndReason, StreamId,
};
use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JObject, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong};

use crate::assets;
use crate::error::AndroidRuntimeError;
use crate::path as path_provider;
use crate::pump::{self, PumpMethods};
use crate::state::{self, RuntimeState};

unsafe extern "Rust" {
    safe fn __istmo_configure_runtime(init: RuntimeInit) -> RuntimeInit;
}

const ILLEGAL_STATE: &str = "java/lang/IllegalStateException";

/// Serialises `nativeStart` so two concurrent callers cannot both get
/// past the "already running" check.
static START_LOCK: Mutex<()> = Mutex::new(());

/// Run an export body, converting panics and caller-bug failures into a
/// pending `IllegalStateException` and returning `fallback` in that case.
fn guarded<'local, R>(
    env: &mut JNIEnv<'local>,
    export: &'static str,
    fallback: R,
    body: impl FnOnce(&mut JNIEnv<'local>) -> Result<R, AndroidRuntimeError>,
) -> R {
    match panic::catch_unwind(AssertUnwindSafe(|| body(env))) {
        Ok(Ok(value)) => value,
        Ok(Err(err)) => {
            report(env, export, &err);
            fallback
        }
        Err(payload) => {
            let message = panic_message(payload.as_ref());
            tracing::error!(export, %message, "istmo jni export panicked");
            throw_illegal_state(env, &format!("istmo {export} panicked: {message}"));
            fallback
        }
    }
}

fn report(env: &mut JNIEnv<'_>, export: &'static str, err: &AndroidRuntimeError) {
    if err.is_benign_race() {
        tracing::debug!(export, %err, "istmo jni export dropped a stale frame");
        return;
    }
    tracing::error!(export, %err, "istmo jni export failed");
    if err.is_java_exception() && env.exception_check().unwrap_or(false) {
        // Let Kotlin see the original Java exception.
        return;
    }
    throw_illegal_state(env, &format!("istmo {export}: {err}"));
}

fn throw_illegal_state(env: &mut JNIEnv<'_>, message: &str) {
    if env.exception_check().unwrap_or(false) {
        return;
    }
    if let Err(err) = env.throw_new(ILLEGAL_STATE, message) {
        tracing::error!(?err, "istmo could not throw IllegalStateException");
    }
}

fn panic_message(payload: &(dyn Any + Send)) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("<non-string panic payload>")
}

/// Ids cross JNI as `jlong` holding the same 64 bits as the Rust `u64`.
const fn wire_id(value: jlong) -> u64 {
    value.cast_unsigned()
}

/// `0` is the Kotlin side's "no instance" sentinel.
const fn instance_from_wire(value: jlong) -> Option<InstanceId> {
    if value == 0 {
        None
    } else {
        Some(InstanceId(wire_id(value)))
    }
}

fn stream_end_reason(
    env: &JNIEnv<'_>,
    reason: jint,
    error_payload: &JByteArray<'_>,
) -> Result<StreamEndReason, AndroidRuntimeError> {
    match reason {
        0 => Ok(StreamEndReason::Complete),
        1 => Ok(StreamEndReason::Cancelled),
        2 => {
            let bytes = if error_payload.is_null() {
                Vec::new()
            } else {
                env.convert_byte_array(error_payload)?
            };
            Ok(StreamEndReason::Error(bytes))
        }
        other => Err(AndroidRuntimeError::InvalidArgument(format!(
            "unknown stream-end reason tag {other}"
        ))),
    }
}

/// Start the runtime and its pump.
///
/// Returns `true` once the runtime is running — also when it already
/// was. Throws `IllegalStateException` (and returns `false`) when
/// startup fails or the runtime was shut down.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeStart<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    runtime_class: JClass<'local>,
) -> jboolean {
    guarded(&mut env, "nativeStart", JNI_FALSE, |env| {
        start(env, &runtime_class).map(|()| JNI_TRUE)
    })
}

fn start(env: &mut JNIEnv<'_>, runtime_class: &JClass<'_>) -> Result<(), AndroidRuntimeError> {
    let _guard = START_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if let Ok(state) = state::get() {
        return if state.is_shut_down() {
            Err(AndroidRuntimeError::ShutDown)
        } else {
            Ok(())
        };
    }

    // Everything that can fail on the JNI side, attaching the pump
    // thread included, happens before the core runtime is installed:
    // once `Runtime::init` succeeds it cannot be undone, so a failure
    // after it would leave a runtime with no pump.
    let class_ref = env.new_global_ref(runtime_class)?;
    let methods = PumpMethods::resolve(env, &class_ref)?;
    let jvm = env.get_java_vm()?;
    let pump = pump::spawn(jvm, class_ref, methods).map_err(AndroidRuntimeError::Pump)?;

    let init = Runtime::init(RuntimeConfig::inline())?;
    let runtime = init.runtime.clone();
    let init = __istmo_configure_runtime(init);
    let (handles, remote_sender) = pump.start(init.outbound);

    runtime.install_remote_envelope_sink(std::sync::Arc::new(move |bytes: Vec<u8>| {
        if let Err(err) = remote_sender.send(bytes) {
            tracing::warn!(?err, "istmo remote sink send failed; pump gone?");
        }
    }));

    state::install(RuntimeState::new(handles.shutdown, handles.join))
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    call_id: jlong,
    ok: jboolean,
    payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitResponse", (), |env| {
        let bytes = env.convert_byte_array(&payload)?;
        let result = if ok == JNI_TRUE {
            Ok(bytes)
        } else {
            Err(bytes)
        };
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: CallId(wire_id(call_id)),
            result,
        }))?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    stream_id: jlong,
    payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitEvent", (), |env| {
        let bytes = env.convert_byte_array(&payload)?;
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Event {
            stream_id: StreamId(wire_id(stream_id)),
            payload: bytes,
        }))?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    stream_id: jlong,
    reason: jint,
    error_payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitStreamEnd", (), |env| {
        let reason = stream_end_reason(env, reason, &error_payload)?;
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::StreamEnd {
            stream_id: StreamId(wire_id(stream_id)),
            reason,
        }))?;
        Ok(())
    });
}

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
    guarded(&mut env, "nativeSubmitCall", (), |env| {
        let plugin_id: String = env.get_string(&plugin_id)?.into();
        let method: String = env.get_string(&method)?.into();
        let payload = env.convert_byte_array(&payload)?;
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Call {
            call_id: CallId(wire_id(call_id)),
            plugin_id,
            instance_id: instance_from_wire(instance_id),
            method,
            payload,
        }))?;
        Ok(())
    });
}

/// Cooperatively cancel a Rust-hosted call that Kotlin started through
/// `nativeSubmitCall`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCancel<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    call_id: jlong,
) {
    guarded(&mut env, "nativeSubmitCancel", (), |_env| {
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Cancel {
            call_id: CallId(wire_id(call_id)),
        }))?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitNotify<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    plugin_id: JString<'local>,
    instance_id: jlong,
    method: JString<'local>,
    payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitNotify", (), |env| {
        let plugin_id: String = env.get_string(&plugin_id)?.into();
        let method: String = env.get_string(&method)?.into();
        let payload = env.convert_byte_array(&payload)?;
        Runtime::global()?.dispatch_inbound(Envelope::new(Frame::Notify {
            plugin_id,
            instance_id: instance_from_wire(instance_id),
            method,
            payload,
        }))?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    channel: JString<'local>,
    payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitEarlyLatest", (), |env| {
        let channel: String = env.get_string(&channel)?.into();
        let bytes = env.convert_byte_array(&payload)?;
        Runtime::submit_early_event(channel, EarlyEventKind::Latest, bytes)?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyQueue<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    channel: JString<'local>,
    capacity: jint,
    payload: JByteArray<'local>,
) {
    guarded(&mut env, "nativeSubmitEarlyQueue", (), |env| {
        let channel: String = env.get_string(&channel)?.into();
        let bytes = env.convert_byte_array(&payload)?;
        let capacity = u32::try_from(capacity).unwrap_or(0);
        Runtime::submit_early_event(channel, EarlyEventKind::Queue { capacity }, bytes)?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeInjectEnvelope<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    bytes: JByteArray<'local>,
) {
    guarded(&mut env, "nativeInjectEnvelope", (), |env| {
        let payload = env.convert_byte_array(&bytes)?;
        Runtime::global()?.inject_wire_envelope(&payload)?;
        Ok(())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeInstallAssets<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    assets: JObject<'local>,
) {
    guarded(&mut env, "nativeInstallAssets", (), |env| {
        assets::install_from_java(env, &assets).map_err(AndroidRuntimeError::Assets)
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeInstallPaths<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
    files_dir: JString<'local>,
    cache_dir: JString<'local>,
    no_backup_dir: JString<'local>,
    external_files_dir: JString<'local>,
) {
    guarded(&mut env, "nativeInstallPaths", (), |env| {
        install_paths(
            env,
            &files_dir,
            &cache_dir,
            &no_backup_dir,
            &external_files_dir,
        )
    });
}

fn install_paths(
    env: &mut JNIEnv<'_>,
    files: &JString<'_>,
    cache: &JString<'_>,
    no_backup: &JString<'_>,
    external_files: &JString<'_>,
) -> Result<(), AndroidRuntimeError> {
    let files: String = env.get_string(files)?.into();
    let cache: String = env.get_string(cache)?.into();
    let no_backup: String = env.get_string(no_backup)?.into();
    let external = if external_files.is_null() {
        None
    } else {
        let s: String = env.get_string(external_files)?.into();
        if s.is_empty() {
            None
        } else {
            Some(std::path::PathBuf::from(s))
        }
    };
    path_provider::install_from_dirs(
        std::path::PathBuf::from(files),
        std::path::PathBuf::from(cache),
        std::path::PathBuf::from(no_backup),
        external,
    );
    Ok(())
}

/// Cancel pending operations and stop the pump. Idempotent, and a no-op
/// before `nativeStart`. The runtime cannot be restarted afterwards.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown<'local>(
    mut env: JNIEnv<'local>,
    _caller: JClass<'local>,
) {
    guarded(&mut env, "nativeShutdown", (), |_env| {
        shutdown();
        Ok(())
    });
}

fn shutdown() {
    let Ok(state) = state::get() else {
        return;
    };
    if !state.begin_shutdown() {
        return;
    }
    if let Ok(runtime) = Runtime::global() {
        runtime.shutdown();
    }
    // Dropping the sender wakes the pump's selector with a disconnect.
    state
        .pump_shutdown
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let handle = state
        .pump_join
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    if let Some(handle) = handle {
        // A Kotlin callback running on the pump thread may call
        // `shutdown`; joining ourselves would deadlock. The pump exits on
        // its own once the callback returns.
        if handle.thread().id() == std::thread::current().id() {
            return;
        }
        if handle.join().is_err() {
            tracing::error!("istmo pump thread panicked");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wire_ids_round_trip_all_64_bits() {
        assert_eq!(wire_id(0), 0);
        assert_eq!(wire_id(42), 42);
        assert_eq!(wire_id(-1), u64::MAX);
        assert_eq!(wire_id(i64::MIN), 1 << 63);
    }

    #[test]
    fn zero_instance_id_means_none() {
        assert_eq!(instance_from_wire(0), None);
        assert_eq!(instance_from_wire(9), Some(InstanceId(9)));
        assert_eq!(instance_from_wire(-1), Some(InstanceId(u64::MAX)));
    }

    #[test]
    fn panic_message_reads_str_and_string_payloads() {
        let s: Box<dyn Any + Send> = Box::new("boom");
        assert_eq!(panic_message(s.as_ref()), "boom");
        let owned: Box<dyn Any + Send> = Box::new(String::from("bang"));
        assert_eq!(panic_message(owned.as_ref()), "bang");
        let other: Box<dyn Any + Send> = Box::new(7_u8);
        assert_eq!(panic_message(other.as_ref()), "<non-string panic payload>");
    }

    #[test]
    fn shutdown_before_start_is_a_no_op() {
        shutdown();
    }
}
