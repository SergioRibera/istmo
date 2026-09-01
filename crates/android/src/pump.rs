//! Outbound frame pump.
//!
//! Drains the runtime's outbound channel, decomposes each `Frame` variant into
//! typed arguments and calls the matching static method on the Kotlin runtime
//! class. This keeps the bincode envelope on the Rust side — Kotlin plugin
//! implementations only see decoded primitives plus the raw method payload
//! (which is itself bincode-encoded, but per plugin, and much simpler in shape).

use std::sync::Arc;
use std::thread::{self, JoinHandle};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, unbounded};
use istmo_core::{Envelope, Frame};
use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{GlobalRef, JObject, JValue};
use jni::signature::{Primitive, ReturnType};

/// Bincode config matching `istmo-core`'s wire codec (unused here — this file
/// only reads pre-decoded frames — but kept for future outbound-encoding
/// helpers such as `nativeSubmitEvent` chains).
#[allow(dead_code)]
const fn codec() -> bincode::config::Configuration {
    bincode::config::standard()
}

pub(crate) struct PumpHandles {
    pub(crate) shutdown: FlumeSender<()>,
    pub(crate) join: JoinHandle<()>,
}

pub(crate) fn spawn(
    jvm: Arc<JavaVM>,
    runtime_class: GlobalRef,
    outbound: FlumeReceiver<Envelope>,
) -> PumpHandles {
    let (shutdown_tx, shutdown_rx) = unbounded::<()>();
    let join = thread::Builder::new()
        .name("istmo-android-pump".to_owned())
        .spawn(move || pump_loop(&jvm, &runtime_class, &outbound, &shutdown_rx))
        .expect("spawn istmo pump thread");
    PumpHandles {
        shutdown: shutdown_tx,
        join,
    }
}

fn pump_loop(
    jvm: &Arc<JavaVM>,
    runtime_class: &GlobalRef,
    outbound: &FlumeReceiver<Envelope>,
    shutdown: &FlumeReceiver<()>,
) {
    let mut env = match jvm.attach_current_thread_as_daemon() {
        Ok(env) => env,
        Err(err) => {
            tracing::error!(?err, "istmo pump failed to attach jni thread");
            return;
        }
    };

    let methods = match PumpMethods::resolve(&mut env, runtime_class) {
        Ok(m) => m,
        Err(err) => {
            tracing::error!(?err, "istmo pump failed to resolve static methods");
            return;
        }
    };

    loop {
        let step = flume::Selector::new()
            .recv(shutdown, |_| PumpStep::Shutdown)
            .recv(outbound, |envelope| {
                envelope.map_or(PumpStep::Shutdown, PumpStep::Deliver)
            })
            .wait();

        let envelope = match step {
            PumpStep::Shutdown => break,
            PumpStep::Deliver(env) => env,
        };

        if let Err(err) = deliver(&mut env, runtime_class, &methods, envelope.frame) {
            tracing::error!(?err, "istmo pump deliver failure");
            let _ = env.exception_clear();
        }
    }
    tracing::info!("istmo pump thread exiting");
}

enum PumpStep {
    Deliver(Envelope),
    Shutdown,
}

/// Cached ids for the four static methods the pump invokes on the Kotlin
/// runtime class.
#[allow(clippy::struct_field_names)]
struct PumpMethods {
    on_call: jni::objects::JStaticMethodID,
    on_cancel: jni::objects::JStaticMethodID,
    on_create_instance: jni::objects::JStaticMethodID,
    on_destroy_instance: jni::objects::JStaticMethodID,
}

impl PumpMethods {
    fn resolve(env: &mut JNIEnv<'_>, class: &GlobalRef) -> Result<Self, jni::errors::Error> {
        Ok(Self {
            on_call: env.get_static_method_id(
                class,
                "onCall",
                "(JLjava/lang/String;JLjava/lang/String;[B)V",
            )?,
            on_cancel: env.get_static_method_id(class, "onCancel", "(J)V")?,
            on_create_instance: env.get_static_method_id(
                class,
                "onCreateInstance",
                "(JLjava/lang/String;[B)V",
            )?,
            on_destroy_instance: env.get_static_method_id(class, "onDestroyInstance", "(J)V")?,
        })
    }
}

#[allow(clippy::cast_possible_wrap)]
fn deliver(
    env: &mut JNIEnv<'_>,
    class: &GlobalRef,
    methods: &PumpMethods,
    frame: Frame,
) -> Result<(), jni::errors::Error> {
    match frame {
        Frame::Call {
            call_id,
            plugin_id,
            instance_id,
            method,
            payload,
        } => {
            let plugin_id_j = env.new_string(&plugin_id)?;
            let method_j = env.new_string(&method)?;
            let payload_j = env.byte_array_from_slice(&payload)?;
            let instance_id_j = instance_id.map_or(0i64, |id| id.get() as i64);
            call_static_void(
                env,
                class,
                methods.on_call,
                &[
                    JValue::Long(call_id.get() as i64).as_jni(),
                    JValue::Object(&JObject::from(plugin_id_j)).as_jni(),
                    JValue::Long(instance_id_j).as_jni(),
                    JValue::Object(&JObject::from(method_j)).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
        }
        Frame::Cancel { call_id } => call_static_void(
            env,
            class,
            methods.on_cancel,
            &[JValue::Long(call_id.get() as i64).as_jni()],
        ),
        Frame::CreateInstance {
            call_id,
            plugin_id,
            payload,
        } => {
            let plugin_id_j = env.new_string(&plugin_id)?;
            let payload_j = env.byte_array_from_slice(&payload)?;
            call_static_void(
                env,
                class,
                methods.on_create_instance,
                &[
                    JValue::Long(call_id.get() as i64).as_jni(),
                    JValue::Object(&JObject::from(plugin_id_j)).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
        }
        Frame::DestroyInstance { instance_id } => call_static_void(
            env,
            class,
            methods.on_destroy_instance,
            &[JValue::Long(instance_id.get() as i64).as_jni()],
        ),
        Frame::Respond { .. } | Frame::Event { .. } | Frame::StreamEnd { .. } => {
            tracing::warn!("pump received inbound-only frame variant; skipping");
            Ok(())
        }
    }
}

fn call_static_void(
    env: &mut JNIEnv<'_>,
    class: &GlobalRef,
    method_id: jni::objects::JStaticMethodID,
    args: &[jni::sys::jvalue],
) -> Result<(), jni::errors::Error> {
    // SAFETY: method_id matches the signature encoded by callers, arg types
    // and count line up with that signature.
    unsafe {
        env.call_static_method_unchecked(
            class,
            method_id,
            ReturnType::Primitive(Primitive::Void),
            args,
        )?;
    }
    Ok(())
}
