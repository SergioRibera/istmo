use std::io;
use std::thread::{self, JoinHandle};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, unbounded};
use istmo_core::{Envelope, Frame};
use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{GlobalRef, JStaticMethodID, JString, JValue};
use jni::signature::{Primitive, ReturnType};

/// Local references one delivery may create: at most three arguments
/// (`Call` / `Notify`) plus the throwable and its `toString` result
/// when the callback throws.
const DELIVERY_LOCAL_CAPACITY: i32 = 6;

pub(crate) struct PumpHandles {
    pub(crate) shutdown: FlumeSender<()>,
    pub(crate) join: JoinHandle<()>,
}

pub(crate) type RemoteEnvelopeSender = flume::Sender<Vec<u8>>;

/// A pump thread already attached to the JVM, parked until it is handed
/// the runtime's outbound receiver.
///
/// Splitting attach from start lets `nativeStart` finish every fallible
/// JNI step before the core runtime is installed. Dropping it unstarted
/// lets the thread exit on its own.
pub(crate) struct AttachedPump {
    outbound_tx: FlumeSender<FlumeReceiver<Envelope>>,
    shutdown: FlumeSender<()>,
    remote: RemoteEnvelopeSender,
    join: JoinHandle<()>,
}

impl AttachedPump {
    /// Hand the pump its outbound receiver and let it start delivering.
    pub(crate) fn start(
        self,
        outbound: FlumeReceiver<Envelope>,
    ) -> (PumpHandles, RemoteEnvelopeSender) {
        // The pump stays parked on the other end until this sender is
        // dropped; the send can only fail if the thread died, which
        // shutdown already tolerates.
        let _ = self.outbound_tx.send(outbound);
        (
            PumpHandles {
                shutdown: self.shutdown,
                join: self.join,
            },
            self.remote,
        )
    }
}

impl std::fmt::Debug for AttachedPump {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AttachedPump").finish_non_exhaustive()
    }
}

/// Spawn the pump thread and wait until it has attached to the JVM, so
/// an attach failure surfaces to `nativeStart` before the core runtime
/// is installed.
pub(crate) fn spawn(
    jvm: JavaVM,
    runtime_class: GlobalRef,
    methods: PumpMethods,
) -> io::Result<AttachedPump> {
    let (shutdown_tx, shutdown_rx) = unbounded::<()>();
    let (remote_tx, remote_rx) = unbounded::<Vec<u8>>();
    let (outbound_tx, outbound_rx) = flume::bounded::<FlumeReceiver<Envelope>>(1);
    let (ready_tx, ready_rx) = flume::bounded::<Result<(), jni::errors::Error>>(1);
    let pump = Pump {
        class: runtime_class,
        methods,
    };
    let join = thread::Builder::new()
        .name("istmo-android-pump".to_owned())
        .spawn(move || {
            let mut env = match jvm.attach_current_thread_as_daemon() {
                Ok(env) => env,
                Err(err) => {
                    let _ = ready_tx.send(Err(err));
                    return;
                }
            };
            let _ = ready_tx.send(Ok(()));
            // Disconnected: startup failed after the attach and the
            // `AttachedPump` was dropped unstarted.
            let Ok(outbound) = outbound_rx.recv() else {
                return;
            };
            pump.run(&mut env, &outbound, &remote_rx, &shutdown_rx);
        })?;
    match ready_rx.recv() {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            let _ = join.join();
            return Err(io::Error::other(format!(
                "istmo pump failed to attach jni thread: {err}"
            )));
        }
        Err(flume::RecvError::Disconnected) => {
            let _ = join.join();
            return Err(io::Error::other(
                "istmo pump thread exited before attaching",
            ));
        }
    }
    Ok(AttachedPump {
        outbound_tx,
        shutdown: shutdown_tx,
        remote: remote_tx,
        join,
    })
}

enum PumpStep {
    Deliver(Envelope),
    DeliverRemote(Vec<u8>),
    Shutdown,
}

/// Static callbacks on `dev.istmo.runtime.IstmoRuntime`, resolved once
/// on the thread calling `nativeStart` so a signature mismatch fails
/// startup loudly.
#[allow(
    clippy::struct_field_names,
    reason = "every field is a callback named after its Kotlin method"
)]
pub(crate) struct PumpMethods {
    on_call: JStaticMethodID,
    on_cancel: JStaticMethodID,
    on_create_instance: JStaticMethodID,
    on_destroy_instance: JStaticMethodID,
    on_respond: JStaticMethodID,
    on_event: JStaticMethodID,
    on_stream_end: JStaticMethodID,
    on_release_native_handle: JStaticMethodID,
    on_notify: JStaticMethodID,
    on_remote_envelope: JStaticMethodID,
}

impl PumpMethods {
    pub(crate) fn resolve(
        env: &mut JNIEnv<'_>,
        class: &GlobalRef,
    ) -> Result<Self, jni::errors::Error> {
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

impl std::fmt::Debug for PumpMethods {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PumpMethods").finish_non_exhaustive()
    }
}

/// Owns everything the pump thread needs to call back into Kotlin.
struct Pump {
    class: GlobalRef,
    methods: PumpMethods,
}

impl Pump {
    fn run(
        &self,
        env: &mut JNIEnv<'_>,
        outbound: &FlumeReceiver<Envelope>,
        remote: &FlumeReceiver<Vec<u8>>,
        shutdown: &FlumeReceiver<()>,
    ) {
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

            // The pump never returns to Java, so local references are
            // only freed when the frame they live in is popped. One
            // frame per delivery keeps the local table bounded.
            let delivered = match step {
                PumpStep::Shutdown => break,
                PumpStep::Deliver(envelope) => {
                    env.with_local_frame(DELIVERY_LOCAL_CAPACITY, |env| {
                        let result = self.deliver(env, envelope.frame);
                        report_failure(env, "deliver", result);
                        Ok::<(), jni::errors::Error>(())
                    })
                }
                PumpStep::DeliverRemote(bytes) => {
                    env.with_local_frame(DELIVERY_LOCAL_CAPACITY, |env| {
                        let result = self.deliver_remote(env, &bytes);
                        report_failure(env, "deliver_remote", result);
                        Ok::<(), jni::errors::Error>(())
                    })
                }
            };
            if let Err(err) = delivered {
                tracing::error!(?err, "istmo pump could not push a jni local frame");
                if let Some(exception) = take_pending_exception(env) {
                    tracing::error!(%exception, "istmo pump local frame exception");
                }
            }
        }
        tracing::info!("istmo pump thread exiting");
    }

    fn deliver_remote(&self, env: &mut JNIEnv<'_>, bytes: &[u8]) -> Result<(), jni::errors::Error> {
        let payload_j = env.byte_array_from_slice(bytes)?;
        self.call_static_void(
            env,
            self.methods.on_remote_envelope,
            &[JValue::Object(&payload_j).as_jni()],
        )
    }

    #[allow(
        clippy::cast_possible_wrap,
        reason = "ids cross JNI as the same 64 bits; Kotlin reads them back as Long"
    )]
    #[allow(
        clippy::too_many_lines,
        reason = "one flat arm per Frame variant reads better than splitting"
    )]
    fn deliver(&self, env: &mut JNIEnv<'_>, frame: Frame) -> Result<(), jni::errors::Error> {
        let methods = &self.methods;
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
                self.call_static_void(
                    env,
                    methods.on_call,
                    &[
                        JValue::Long(call_id.get() as i64).as_jni(),
                        JValue::Object(&plugin_id_j).as_jni(),
                        JValue::Long(instance_id_j).as_jni(),
                        JValue::Object(&method_j).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
                    ],
                )
            }
            Frame::Cancel { call_id } => self.call_static_void(
                env,
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
                self.call_static_void(
                    env,
                    methods.on_create_instance,
                    &[
                        JValue::Long(call_id.get() as i64).as_jni(),
                        JValue::Object(&plugin_id_j).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
                    ],
                )
            }
            Frame::DestroyInstance { instance_id } => self.call_static_void(
                env,
                methods.on_destroy_instance,
                &[JValue::Long(instance_id.get() as i64).as_jni()],
            ),
            Frame::Respond { call_id, result } => {
                let (ok, payload) = match result {
                    Ok(bytes) => (true, bytes),
                    Err(bytes) => (false, bytes),
                };
                let payload_j = env.byte_array_from_slice(&payload)?;
                self.call_static_void(
                    env,
                    methods.on_respond,
                    &[
                        JValue::Long(call_id.get() as i64).as_jni(),
                        JValue::Bool(u8::from(ok)).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
                    ],
                )
            }
            Frame::Event { stream_id, payload } => {
                let payload_j = env.byte_array_from_slice(&payload)?;
                self.call_static_void(
                    env,
                    methods.on_event,
                    &[
                        JValue::Long(stream_id.get() as i64).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
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
                self.call_static_void(
                    env,
                    methods.on_stream_end,
                    &[
                        JValue::Long(stream_id.get() as i64).as_jni(),
                        JValue::Int(reason_tag).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
                    ],
                )
            }
            Frame::EarlyEvent { channel, .. } => {
                tracing::warn!(
                    channel = %channel,
                    "dropped outbound EarlyEvent frame; variant is inbound-only",
                );
                Ok(())
            }
            Frame::ReleaseNativeHandle { handle_id } => self.call_static_void(
                env,
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
                self.call_static_void(
                    env,
                    methods.on_notify,
                    &[
                        JValue::Object(&plugin_id_j).as_jni(),
                        JValue::Long(instance_id_j).as_jni(),
                        JValue::Object(&method_j).as_jni(),
                        JValue::Object(&payload_j).as_jni(),
                    ],
                )
            }
        }
    }

    fn call_static_void(
        &self,
        env: &mut JNIEnv<'_>,
        method_id: JStaticMethodID,
        args: &[jni::sys::jvalue],
    ) -> Result<(), jni::errors::Error> {
        // SAFETY: `method_id` was resolved by `PumpMethods::resolve` on
        // `self.class`, which the `GlobalRef` keeps loaded, so the id
        // stays valid. Every caller passes `args` whose count and JNI
        // types match the signature string that method was resolved
        // with, and the return type is `void` for all of them.
        unsafe {
            env.call_static_method_unchecked(
                &self.class,
                method_id,
                ReturnType::Primitive(Primitive::Void),
                args,
            )?;
        }
        Ok(())
    }
}

/// Log a failed delivery, including the Java exception a Kotlin
/// callback threw, and clear it so the pump keeps running.
fn report_failure(
    env: &mut JNIEnv<'_>,
    what: &'static str,
    result: Result<(), jni::errors::Error>,
) {
    let Err(err) = result else { return };
    if let Some(exception) = take_pending_exception(env) {
        tracing::error!(?err, %exception, "istmo pump {what} failure");
    } else {
        tracing::error!(?err, "istmo pump {what} failure");
    }
}

/// Clear the pending Java exception, if any, and render it for logs.
///
/// The stack trace goes to logcat through `ExceptionDescribe`; the
/// returned `toString()` lands in the `tracing` event.
fn take_pending_exception(env: &mut JNIEnv<'_>) -> Option<String> {
    if !env.exception_check().unwrap_or(false) {
        return None;
    }
    let throwable = env.exception_occurred().ok();
    let _ = env.exception_describe();
    let _ = env.exception_clear();
    let throwable = throwable.filter(|t| !t.is_null())?;
    let rendered = env
        .call_method(&throwable, "toString", "()Ljava/lang/String;", &[])
        .and_then(jni::objects::JValueGen::l)
        .and_then(|text| env.get_string(&JString::from(text)).map(String::from));
    Some(rendered.unwrap_or_else(|_| {
        let _ = env.exception_clear();
        "<java exception without printable toString>".to_owned()
    }))
}
