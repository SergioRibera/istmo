//! Rust → Kotlin calls made on the Rust caller's own thread.
//!
//! The thread attaches to the JVM on its first call and stays attached
//! until it exits (the `jni` crate detaches it from a thread-local
//! guard): attaching and detaching per call would cost more than the
//! pump hop this replaces.

use std::collections::HashMap;
use std::sync::Mutex;

use istmo_core::{CallId, DirectOutcome, DirectPeer, InstanceId, StreamId};
use jni::JNIEnv;
use jni::JavaVM;
use jni::objects::{GlobalRef, JByteArray, JStaticMethodID, JValue};
use jni::signature::{Primitive, ReturnType};

use crate::pump::take_pending_exception;

/// Local references one direct call creates: the payload array and the
/// returned reply, plus the throwable and its text on failure.
const CALL_LOCAL_CAPACITY: i32 = 6;

/// Most distinct plugin ids and method names kept as Java strings.
const MAX_INTERNED: usize = 1024;

/// Delivers calls through `IstmoRuntime.onCallDirect`, which runs the
/// Kotlin backend in place and returns its answer when it did not
/// suspend.
pub(crate) struct AndroidPeer {
    jvm: JavaVM,
    class: GlobalRef,
    on_call_direct: JStaticMethodID,
    on_event: JStaticMethodID,
    /// Plugin ids and method names as Java strings: built once instead
    /// of per call.
    names: Mutex<HashMap<String, GlobalRef>>,
}

impl std::fmt::Debug for AndroidPeer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AndroidPeer").finish_non_exhaustive()
    }
}

impl AndroidPeer {
    /// Resolve `onCallDirect` and `onEvent` on `class` (the runtime class
    /// `nativeStart` received).
    pub(crate) fn resolve(
        env: &mut JNIEnv<'_>,
        class: GlobalRef,
    ) -> Result<Self, jni::errors::Error> {
        let on_call_direct = env.get_static_method_id(
            &class,
            "onCallDirect",
            "(JLjava/lang/String;JLjava/lang/String;[B)[B",
        )?;
        let on_event = env.get_static_method_id(&class, "onEvent", "(J[B)V")?;
        Ok(Self {
            jvm: env.get_java_vm()?,
            class,
            on_call_direct,
            on_event,
            names: Mutex::new(HashMap::new()),
        })
    }

    #[allow(
        clippy::cast_possible_wrap,
        reason = "ids cross JNI as the same 64 bits; Kotlin reads them back as Long"
    )]
    fn deliver_event(
        &self,
        env: &mut JNIEnv<'_>,
        stream_id: StreamId,
        payload: &[u8],
    ) -> Result<(), jni::errors::Error> {
        let payload_j = env.byte_array_from_slice(payload)?;
        // SAFETY: `on_event` was resolved on `self.class` (kept loaded by
        // the `GlobalRef`) with signature `(J[B)V`; the arguments match.
        unsafe {
            env.call_static_method_unchecked(
                &self.class,
                self.on_event,
                ReturnType::Primitive(Primitive::Void),
                &[
                    JValue::Long(stream_id.get() as i64).as_jni(),
                    JValue::Object(&payload_j).as_jni(),
                ],
            )?;
        }
        Ok(())
    }

    /// `name` as a global Java string.
    fn interned(&self, env: &JNIEnv<'_>, name: &str) -> Result<GlobalRef, jni::errors::Error> {
        let found = istmo_core::sync::lock(&self.names).get(name).cloned();
        if let Some(found) = found {
            return Ok(found);
        }
        let local = env.new_string(name)?;
        let global = env.new_global_ref(&local)?;
        env.delete_local_ref(local)?;
        let mut names = istmo_core::sync::lock(&self.names);
        if names.len() < MAX_INTERNED {
            names.insert(name.to_owned(), global.clone());
        }
        drop(names);
        Ok(global)
    }

    #[allow(
        clippy::cast_possible_wrap,
        reason = "ids cross JNI as the same 64 bits; Kotlin reads them back as Long"
    )]
    fn deliver(
        &self,
        env: &mut JNIEnv<'_>,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: &str,
        payload: &[u8],
    ) -> Result<DirectOutcome, jni::errors::Error> {
        let plugin_id_j = self.interned(env, plugin_id)?;
        let method_j = self.interned(env, method)?;
        let payload_j = env.byte_array_from_slice(payload)?;
        let instance_id_j = instance_id.map_or(0i64, |id| id.get() as i64);
        probe!(DirectCallingKotlin);
        // SAFETY: `on_call_direct` was resolved on `self.class`, which the
        // `GlobalRef` keeps loaded; the arguments match its signature
        // `(JLjava/lang/String;JLjava/lang/String;[B)[B` and the return
        // type is an object.
        let returned = unsafe {
            env.call_static_method_unchecked(
                &self.class,
                self.on_call_direct,
                ReturnType::Object,
                &[
                    JValue::Long(call_id.get() as i64).as_jni(),
                    JValue::Object(plugin_id_j.as_obj()).as_jni(),
                    JValue::Long(instance_id_j).as_jni(),
                    JValue::Object(method_j.as_obj()).as_jni(),
                    JValue::Object(&payload_j).as_jni(),
                ],
            )
        };
        probe!(DirectKotlinReturned);
        let reply = returned?.l()?;
        if reply.is_null() {
            return Ok(DirectOutcome::Pending);
        }
        let bytes = crate::jni_exports::read_array(env, &JByteArray::from(reply))?;
        Ok(DirectOutcome::Answered(Ok(bytes)))
    }
}

impl DirectPeer for AndroidPeer {
    fn call(
        &self,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: &str,
        payload: &[u8],
    ) -> DirectOutcome {
        let Ok(mut env) = self.jvm.attach_current_thread_as_daemon() else {
            return DirectOutcome::Unavailable;
        };
        // A thread with a Java exception pending (a native method that
        // has not returned to Kotlin yet) may not call into the JVM.
        if env.exception_check().unwrap_or(true) {
            return DirectOutcome::Unavailable;
        }
        let delivered = env.with_local_frame(CALL_LOCAL_CAPACITY, |env| {
            match self.deliver(env, call_id, plugin_id, instance_id, method, payload) {
                Ok(outcome) => Ok::<_, jni::errors::Error>(outcome),
                Err(err) => {
                    // `onCallDirect` answers its own failures; this is a
                    // JNI failure around it (out of memory, a missing
                    // class). The call may have started: answer it here
                    // rather than send it a second time.
                    let thrown = take_pending_exception(env);
                    tracing::error!(
                        ?err,
                        ?thrown,
                        plugin_id,
                        method,
                        "direct Kotlin call failed"
                    );
                    Ok(DirectOutcome::Answered(Err(istmo_core::codec::encode(
                        &format!("Kotlin peer call failed: {err}"),
                    )
                    .unwrap_or_default())))
                }
            }
        });
        delivered.unwrap_or_else(|err| {
            tracing::error!(?err, "no JNI local frame for a direct Kotlin call");
            DirectOutcome::Unavailable
        })
    }

    fn event(&self, stream_id: StreamId, payload: &[u8]) -> bool {
        let Ok(mut env) = self.jvm.attach_current_thread_as_daemon() else {
            return false;
        };
        if env.exception_check().unwrap_or(true) {
            return false;
        }
        let delivered = env.with_local_frame(CALL_LOCAL_CAPACITY, |env| {
            if let Err(err) = self.deliver_event(env, stream_id, payload) {
                // `onEvent` never throws; a JNI failure around it may have
                // delivered the item already, so it is not sent twice.
                let thrown = take_pending_exception(env);
                tracing::error!(?err, ?thrown, ?stream_id, "direct Kotlin event failed");
            }
            Ok::<(), jni::errors::Error>(())
        });
        // No local frame: nothing reached Kotlin, queue it instead.
        delivered.is_ok()
    }
}
