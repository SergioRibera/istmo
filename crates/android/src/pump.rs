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

/// Sender half of the remote-envelope byte channel; the runtime's remote
/// sink pushes into it, the pump thread drains it and forwards bytes via
/// `onRemoteEnvelope`.
pub(crate) type RemoteEnvelopeSender = flume::Sender<Vec<u8>>;

pub(crate) fn spawn(
    jvm: Arc<JavaVM>,
    runtime_class: GlobalRef,
    outbound: FlumeReceiver<Envelope>,
) -> (PumpHandles, RemoteEnvelopeSender) {
    let (shutdown_tx, shutdown_rx) = unbounded::<()>();
    let (remote_tx, remote_rx) = unbounded::<Vec<u8>>();
    let join = thread::Builder::new()
        .name("istmo-android-pump".to_owned())
        .spawn(move || pump_loop(&jvm, &runtime_class, &outbound, &remote_rx, &shutdown_rx))
        .expect("spawn istmo pump thread");
    (
        PumpHandles { shutdown: shutdown_tx, join },
        remote_tx,
    )
}

fn pump_loop(
    jvm: &Arc<JavaVM>,
    runtime_class: &GlobalRef,
    outbound: &FlumeReceiver<Envelope>,
    remote: &FlumeReceiver<Vec<u8>>,
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
            .recv(remote, |bytes| {
                bytes.map_or(PumpStep::Shutdown, PumpStep::DeliverRemote)
            })
            .wait();

        match step {
            PumpStep::Shutdown => break,
            PumpStep::Deliver(envelope) => {
                if let Err(err) = deliver(&mut env, runtime_class, &methods, envelope.frame) {
                    tracing::error!(?err, "istmo pump deliver failure");
                    let _ = env.exception_clear();
                }
            }
            PumpStep::DeliverRemote(bytes) => {
                if let Err(err) = deliver_remote(&mut env, runtime_class, &methods, &bytes) {
                    tracing::error!(?err, "istmo pump deliver_remote failure");
                    let _ = env.exception_clear();
                }
            }
        }
    }
    tracing::info!("istmo pump thread exiting");
}

enum PumpStep {
    Deliver(Envelope),
    DeliverRemote(Vec<u8>),
    Shutdown,
}

fn deliver_remote(
    env: &mut JNIEnv<'_>,
    class: &GlobalRef,
    methods: &PumpMethods,
    bytes: &[u8],
) -> Result<(), jni::errors::Error> {
    let payload_j = env.byte_array_from_slice(bytes)?;
    call_static_void(
        env,
        class,
        methods.on_remote_envelope,
        &[JValue::Object(&JObject::from(payload_j)).as_jni()],
    )
}

/// Cached ids for every static method the pump invokes on the Kotlin runtime
/// class. Outbound direction is Rust → Kotlin; the pump translates each
/// [`Frame`] variant into a typed static-method call.
#[allow(clippy::struct_field_names)]
struct PumpMethods {
    on_call: jni::objects::JStaticMethodID,
    on_cancel: jni::objects::JStaticMethodID,
    on_create_instance: jni::objects::JStaticMethodID,
    on_destroy_instance: jni::objects::JStaticMethodID,
    on_respond: jni::objects::JStaticMethodID,
    on_event: jni::objects::JStaticMethodID,
    on_stream_end: jni::objects::JStaticMethodID,
    on_release_native_handle: jni::objects::JStaticMethodID,
    on_notify: jni::objects::JStaticMethodID,
    on_remote_envelope: jni::objects::JStaticMethodID,
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
            on_respond: env.get_static_method_id(class, "onRespond", "(JZ[B)V")?,
            on_event: env.get_static_method_id(class, "onEvent", "(J[B)V")?,
            on_stream_end: env.get_static_method_id(class, "onStreamEnd", "(JI[B)V")?,
            on_release_native_handle: env.get_static_method_id(
                class,
                "onReleaseNativeHandle",
                "(J)V",
            )?,
            on_notify: env.get_static_method_id(
                class,
                "onNotify",
                "(Ljava/lang/String;JLjava/lang/String;[B)V",
            )?,
            on_remote_envelope: env.get_static_method_id(class, "onRemoteEnvelope", "([B)V")?,
        })
    }
}

#[allow(clippy::cast_possible_wrap, clippy::too_many_lines)]
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
        Frame::Respond { call_id, result } => {
            let (ok, payload) = match result {
                Ok(bytes) => (true, bytes),
                Err(bytes) => (false, bytes),
            };
            let payload_j = env.byte_array_from_slice(&payload)?;
            call_static_void(
                env,
                class,
                methods.on_respond,
                &[
                    JValue::Long(call_id.get() as i64).as_jni(),
                    JValue::Bool(u8::from(ok)).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
        }
        Frame::Event { stream_id, payload } => {
            let payload_j = env.byte_array_from_slice(&payload)?;
            call_static_void(
                env,
                class,
                methods.on_event,
                &[
                    JValue::Long(stream_id.get() as i64).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
        }
        Frame::StreamEnd { stream_id, reason } => {
            let (reason_tag, payload) = match reason {
                istmo_core::StreamEndReason::Complete => (0i32, Vec::new()),
                istmo_core::StreamEndReason::Cancelled => (1, Vec::new()),
                istmo_core::StreamEndReason::Error(bytes) => (2, bytes),
            };
            let payload_j = env.byte_array_from_slice(&payload)?;
            call_static_void(
                env,
                class,
                methods.on_stream_end,
                &[
                    JValue::Long(stream_id.get() as i64).as_jni(),
                    JValue::Int(reason_tag).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
        }
        Frame::EarlyEvent { channel, .. } => {
            // EarlyEvent is an inbound-only variant: native publishes into
            // the Rust `EarlyEventStore`. Rust does not ship early events
            // back out to native (Kotlin observes lifecycle/deep-link
            // state through its own channels, not through the frame pump).
            tracing::warn!(
                channel = %channel,
                "dropped outbound EarlyEvent frame; variant is inbound-only",
            );
            Ok(())
        }
        Frame::ReleaseNativeHandle { handle_id } => call_static_void(
            env,
            class,
            methods.on_release_native_handle,
            &[JValue::Long(handle_id.get() as i64).as_jni()],
        ),
        Frame::Notify {
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
                methods.on_notify,
                &[
                    JValue::Object(&JObject::from(plugin_id_j)).as_jni(),
                    JValue::Long(instance_id_j).as_jni(),
                    JValue::Object(&JObject::from(method_j)).as_jni(),
                    JValue::Object(&JObject::from(payload_j)).as_jni(),
                ],
            )
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
