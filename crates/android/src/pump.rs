use std::io;
use std::thread::{self, JoinHandle};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, unbounded};
use istmo_core::{Envelope, Frame};
use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{GlobalRef, JObject, JStaticMethodID, JString, JValue};
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
            // Kotlin callbacks run here and may send: never wait on the
            // channel this thread drains.
            let _delivering = istmo_core::DeliveryScope::enter();
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
    on_events: JStaticMethodID,
    /// `byte[]`, the element class of `onEvents`' batch.
    byte_array_class: GlobalRef,
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
            on_events: env.get_static_method_id(class, "onEvents", "(J[[B)V")?,
            byte_array_class: {
                let local = env.find_class("[B")?;
                env.new_global_ref(local)?
            },
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
        // Started after `Runtime::init`, so the runtime exists.
        let runtime = istmo_core::Runtime::global().ok();
        // A frame taken while collecting a batch of events that did not
        // belong to it: delivered next.
        let mut carry: Option<Envelope> = None;
        loop {
            let step = carry
                .take()
                .map_or_else(|| next_step(outbound, remote, shutdown), PumpStep::Deliver);
            probe!(PumpDequeued);

            // The pump never returns to Java, so local references are
            // only freed when the frame they live in is popped. One
            // frame per delivery keeps the local table bounded.
            let delivered = match step {
                PumpStep::Shutdown => break,
                PumpStep::Deliver(Envelope {
                    frame: Frame::Event { stream_id, payload },
                    ..
                }) => {
                    // Events queued back to back for one stream cross in
                    // one call.
                    let (batch, next) = collect_events(outbound, payload, stream_id);
                    carry = next;
                    let frames = batch.len();
                    let delivered = env.with_local_frame(DELIVERY_LOCAL_CAPACITY, |env| {
                        let result = self.deliver_events(env, stream_id, &batch);
                        report_failure(env, "deliver_events", result);
                        Ok::<(), jni::errors::Error>(())
                    });
                    if let Some(runtime) = &runtime {
                        (0..frames).for_each(|_| runtime.outbound_delivered());
                    }
                    delivered
                }
                PumpStep::Deliver(envelope) => {
                    let delivered = env.with_local_frame(DELIVERY_LOCAL_CAPACITY, |env| {
                        let result = self.deliver(env, envelope.frame);
                        report_failure(env, "deliver", result);
                        Ok::<(), jni::errors::Error>(())
                    });
                    // Calls may go straight to Kotlin again once nothing
                    // queued before them is still on its way.
                    if let Some(runtime) = &runtime {
                        runtime.outbound_delivered();
                    }
                    delivered
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

    /// One event through `onEvent`, several through one `onEvents`.
    #[allow(
        clippy::cast_possible_wrap,
        reason = "ids cross JNI as the same 64 bits; Kotlin reads them back as Long"
    )]
    fn deliver_events(
        &self,
        env: &mut JNIEnv<'_>,
        stream_id: istmo_core::StreamId,
        batch: &[Vec<u8>],
    ) -> Result<(), jni::errors::Error> {
        let stream_j = JValue::Long(stream_id.get() as i64).as_jni();
        if let [single] = batch {
            let payload_j = env.byte_array_from_slice(single)?;
            return self.call_static_void(
                env,
                self.methods.on_event,
                &[stream_j, JValue::Object(&payload_j).as_jni()],
            );
        }
        let len = i32::try_from(batch.len()).unwrap_or(i32::MAX);
        let class: &jni::objects::JClass<'_> = self.methods.byte_array_class.as_obj().into();
        let items = env.new_object_array(len, class, JObject::null())?;
        for (index, item) in (0..len).zip(batch) {
            let item_j = env.byte_array_from_slice(item)?;
            env.set_object_array_element(&items, index, &item_j)?;
            env.delete_local_ref(item_j)?;
        }
        self.call_static_void(
            env,
            self.methods.on_events,
            &[stream_j, JValue::Object(&items).as_jni()],
        )
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
                    // Kotlin only knows the three tags above: end anything newer
                    // as an error so the consumer still sees the stream close.
                    other => {
                        tracing::warn!(?other, "unmapped stream end reason; reported as error");
                        (2, Vec::new())
                    }
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
        probe!(PumpCallingKotlin);
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
        probe!(PumpKotlinReturned);
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
pub(crate) fn take_pending_exception(env: &mut JNIEnv<'_>) -> Option<String> {
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

/// The next thing to deliver: a frame queued right after the last
/// delivery is taken without parking the pump.
fn next_step(
    outbound: &FlumeReceiver<Envelope>,
    remote: &FlumeReceiver<Vec<u8>>,
    shutdown: &FlumeReceiver<()>,
) -> PumpStep {
    static WAIT: istmo_core::spin::SpinWait = istmo_core::spin::SpinWait::new();
    let quick = WAIT.spin(|| {
        outbound
            .try_recv()
            .ok()
            .map(PumpStep::Deliver)
            .or_else(|| remote.try_recv().ok().map(PumpStep::DeliverRemote))
    });
    quick.unwrap_or_else(|| {
        flume::Selector::new()
            .recv(shutdown, |_| PumpStep::Shutdown)
            .recv(outbound, |envelope| {
                envelope.map_or(PumpStep::Shutdown, PumpStep::Deliver)
            })
            .recv(remote, |bytes| {
                bytes.map_or(PumpStep::Shutdown, PumpStep::DeliverRemote)
            })
            .wait()
    })
}

/// Most events one `onEvents` call carries.
const MAX_EVENT_BATCH: usize = 64;

/// `first` plus the events for `stream_id` queued right behind it (up
/// to [`MAX_EVENT_BATCH`]), and the first other frame taken, if any.
fn collect_events(
    outbound: &FlumeReceiver<Envelope>,
    first: Vec<u8>,
    stream_id: istmo_core::StreamId,
) -> (Vec<Vec<u8>>, Option<Envelope>) {
    let mut batch = vec![first];
    while batch.len() < MAX_EVENT_BATCH {
        let Ok(envelope) = outbound.try_recv() else {
            break;
        };
        match envelope.frame {
            Frame::Event {
                stream_id: next,
                payload,
            } if next == stream_id => batch.push(payload),
            frame => {
                return (batch, Some(Envelope { frame, ..envelope }));
            }
        }
    }
    (batch, None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use istmo_core::StreamId;

    fn event(stream: u64, byte: u8) -> Envelope {
        Envelope::new(Frame::Event {
            stream_id: StreamId(stream),
            payload: vec![byte],
        })
    }

    #[test]
    fn back_to_back_events_of_one_stream_batch_and_stop_at_anything_else() {
        let (tx, rx) = flume::unbounded();
        for byte in 1..=3 {
            tx.send(event(7, byte)).unwrap();
        }
        tx.send(event(8, 9)).unwrap();
        tx.send(event(7, 4)).unwrap();
        let (batch, next) = collect_events(&rx, vec![0], StreamId(7));
        assert_eq!(batch, [vec![0], vec![1], vec![2], vec![3]]);
        assert!(matches!(
            next.map(|e| e.frame),
            Some(Frame::Event {
                stream_id: StreamId(8),
                ..
            })
        ));
        let (batch, next) = collect_events(&rx, vec![5], StreamId(7));
        assert_eq!(batch, [vec![5], vec![4]]);
        assert!(next.is_none());
    }

    #[test]
    fn a_batch_is_capped() {
        let (tx, rx) = flume::unbounded();
        for _ in 0..100 {
            tx.send(event(1, 0)).unwrap();
        }
        let (batch, next) = collect_events(&rx, vec![0], StreamId(1));
        assert_eq!(batch.len(), MAX_EVENT_BATCH);
        assert!(next.is_none());
        assert_eq!(rx.len(), 100 - (MAX_EVENT_BATCH - 1));
    }
}
