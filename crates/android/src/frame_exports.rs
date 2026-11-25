//! JNI entries of `dev.istmo.runtime.IstmoFrames`: what the Kotlin
//! `IstmoFrameSender` calls to feed a Rust frame sink.
//!
//! A sender opens one producer handle (`nativeOpenProducer`, an
//! `AndroidProducer` behind an opaque `long`) and submits through it, so
//! a frame costs no registry lookup and no `Runtime::global()`. Frame
//! entries answer with a `SubmitStatus` code and never throw: a panic
//! inside Rust answers `INTERNAL`. Only the sender passes a handle, and
//! it serialises every call against `nativeFreeProducer`, after which it
//! never passes that handle again.

#![expect(
    unsafe_code,
    reason = "producer handles are raw pointers owned by the Kotlin sender"
)]

use std::panic::{self, AssertUnwindSafe};
use std::time::Duration;

use istmo_core::Runtime;
use istmo_core::native_frame::{NATIVE_ABI_VERSION, SubmitStatus};
use jni::JNIEnv;
use jni::objects::{JByteArray, JByteBuffer, JIntArray, JObject, JString};
use jni::sys::{JNI_FALSE, JNI_TRUE, jboolean, jint, jlong, jobjectArray};

use crate::frame_producer::AndroidProducer;
use crate::native_buffer;

/// Metadata up to this size is read onto the stack: no allocation.
const STACK_META: usize = 256;

/// Run a frame entry, answering `on_panic` instead of unwinding into
/// the JVM.
fn frame_guard<R>(export: &'static str, on_panic: R, body: impl FnOnce() -> R) -> R {
    panic::catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        tracing::error!(export, "istmo frame entry panicked");
        on_panic
    })
}

/// The `long` Kotlin keeps for `ptr`: its address bits, top byte (a
/// heap tag on Android) included.
fn ptr_handle(ptr: *mut AndroidProducer) -> jlong {
    u64::try_from(ptr.expose_provenance()).map_or(0, u64::cast_signed)
}

/// The pointer behind `handle`, as [`ptr_handle`] made it.
fn handle_ptr(handle: jlong) -> *mut AndroidProducer {
    usize::try_from(handle.cast_unsigned())
        .map_or(std::ptr::null_mut(), std::ptr::with_exposed_provenance_mut)
}

/// The producer behind `handle`; `None` for the null handle.
///
/// # Safety
///
/// `handle` is 0 or a value `nativeOpenProducer` returned that
/// `nativeFreeProducer` has not freed.
unsafe fn producer<'a>(handle: jlong) -> Option<&'a AndroidProducer> {
    let ptr = handle_ptr(handle).cast_const();
    // SAFETY: forwarded from this function's contract.
    unsafe { ptr.as_ref() }
}

/// Run `body` on the first `len` bytes of `meta`, read without a heap
/// allocation when small.
fn with_meta<R>(
    env: &JNIEnv<'_>,
    meta: &JByteArray<'_>,
    len: jint,
    body: impl FnOnce(&[u8]) -> R,
) -> Result<R, SubmitStatus> {
    if meta.is_null() {
        return Err(SubmitStatus::Invalid);
    }
    let len = usize::try_from(len).map_err(|_| SubmitStatus::Invalid)?;
    let available = env
        .get_array_length(meta)
        .ok()
        .and_then(|n| usize::try_from(n).ok())
        .ok_or(SubmitStatus::Invalid)?;
    if len > available {
        return Err(SubmitStatus::Invalid);
    }
    let mut stack = [0i8; STACK_META];
    let mut heap = Vec::new();
    let buf: &mut [i8] = if len <= STACK_META {
        &mut stack[..len]
    } else {
        heap.resize(len, 0);
        &mut heap
    };
    env.get_byte_array_region(meta, 0, buf)
        .map_err(|_| SubmitStatus::Invalid)?;
    // SAFETY: `i8` and `u8` have the same size and alignment; the slice
    // covers exactly the initialised `buf`.
    let bytes = unsafe { std::slice::from_raw_parts(buf.as_ptr().cast::<u8>(), buf.len()) };
    Ok(body(bytes))
}

fn code(result: Result<SubmitStatus, SubmitStatus>) -> jint {
    result.unwrap_or_else(|status| status).code()
}

/// `(offset, length)` within `capacity`, as `usize`s.
fn span(offset: jint, length: jint, capacity: usize) -> Option<(usize, usize)> {
    let offset = usize::try_from(offset).ok()?;
    let length = usize::try_from(length).ok()?;
    (offset.checked_add(length)? <= capacity).then_some((offset, length))
}

/// The native entry version; `IstmoRuntime.start` refuses a library
/// whose version differs from the one it was built for.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeAbiVersion<'local>(
    _env: JNIEnv<'local>,
    _caller: JObject<'local>,
) -> jint {
    jint::try_from(NATIVE_ABI_VERSION).unwrap_or(jint::MAX)
}

/// Open a producer on sink `sink` presenting metadata `fingerprint`.
/// Returns the handle, or 0 with the reason in `status[0]`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeOpenProducer<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    sink: jlong,
    fingerprint: jlong,
    status: JIntArray<'local>,
) -> jlong {
    let opened = frame_guard("nativeOpenProducer", Err(SubmitStatus::Internal), || {
        let runtime = Runtime::global().map_err(|_| SubmitStatus::NotStarted)?;
        runtime
            .open_native_producer(sink.cast_unsigned(), fingerprint.cast_unsigned())
            .map(|producer| Box::into_raw(Box::new(AndroidProducer::new(producer))))
    });
    let (handle, outcome) = match opened {
        Ok(ptr) => (ptr_handle(ptr), SubmitStatus::Accepted),
        Err(status) => (0, status),
    };
    if !status.is_null() {
        let _ = env.set_int_array_region(&status, 0, &[outcome.code()]);
    }
    handle
}

/// End the stream: completed when `error` is null, failed with its text
/// otherwise. The handle stays valid (for releases) until freed.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeEndProducer<'local>(
    mut env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    error: JString<'local>,
) {
    let message = if error.is_null() {
        None
    } else {
        env.get_string(&error).ok().map(String::from)
    };
    frame_guard("nativeEndProducer", (), || {
        // SAFETY: the sender's handle contract (module docs).
        let Some(producer) = (unsafe { producer(handle) }) else {
            return;
        };
        match message {
            Some(message) => producer.producer().close_with_message(message),
            None => producer.producer().close(),
        }
    });
}

/// Free the producer: it no longer counts as one of the sink's
/// producers. Frames already submitted keep their buffers.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeFreeProducer<'local>(
    _env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
) {
    frame_guard("nativeFreeProducer", (), || {
        let ptr = handle_ptr(handle);
        if !ptr.is_null() {
            // SAFETY: the sender frees each handle once, after its last
            // use (module docs).
            drop(unsafe { Box::from_raw(ptr) });
        }
    });
}

/// Submit an `android.hardware.HardwareBuffer`; Rust takes its own
/// reference. `ticket` (non-zero) comes back from `nativeAwaitReleased`
/// once Rust is done with the buffer, refused or not.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeSubmitHardwareBuffer<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    buffer: JObject<'local>,
    ticket: jlong,
    meta: JByteArray<'local>,
    meta_len: jint,
) -> jint {
    frame_guard(
        "nativeSubmitHardwareBuffer",
        SubmitStatus::Internal.code(),
        || {
            // SAFETY: the sender's handle contract (module docs).
            let Some(producer) = (unsafe { producer(handle) }) else {
                return SubmitStatus::Invalid.code();
            };
            if buffer.is_null() {
                return SubmitStatus::Invalid.code();
            }
            let native = native_buffer::hardware_buffer(&env, &buffer);
            if native.is_err() && ticket != 0 {
                // Every ticket handed over comes back, refused or not.
                producer.released().push(ticket);
            }
            code(native.and_then(|native| {
                let native = producer.ticketed(native, ticket);
                with_meta(&env, &meta, meta_len, |meta| {
                    producer.producer().submit(meta, native)
                })
            }))
        },
    )
}

/// Submit a copy of `length` bytes at `offset` of a heap array.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeSubmitBytes<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    bytes: JByteArray<'local>,
    offset: jint,
    length: jint,
    meta: JByteArray<'local>,
    meta_len: jint,
) -> jint {
    frame_guard("nativeSubmitBytes", SubmitStatus::Internal.code(), || {
        // SAFETY: the sender's handle contract (module docs).
        let Some(producer) = (unsafe { producer(handle) }) else {
            return SubmitStatus::Invalid.code();
        };
        if bytes.is_null() {
            return SubmitStatus::Invalid.code();
        }
        let capacity = env
            .get_array_length(&bytes)
            .ok()
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(0);
        let Some((offset, length)) = span(offset, length, capacity) else {
            return SubmitStatus::Invalid.code();
        };
        let mut data = vec![0i8; length];
        let Ok(start) = jint::try_from(offset) else {
            return SubmitStatus::Invalid.code();
        };
        if env.get_byte_array_region(&bytes, start, &mut data).is_err() {
            return SubmitStatus::Invalid.code();
        }
        let data: Vec<u8> = data.into_iter().map(i8::cast_unsigned).collect();
        code(with_meta(&env, &meta, meta_len, |meta| {
            producer.submit_copy(meta, &data)
        }))
    })
}

/// Submit a copy of `length` bytes at `offset` of a direct
/// `ByteBuffer`: the JVM keeps owning (and may reuse) its memory.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeSubmitDirectBuffer<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    buffer: JByteBuffer<'local>,
    offset: jint,
    length: jint,
    meta: JByteArray<'local>,
    meta_len: jint,
) -> jint {
    frame_guard(
        "nativeSubmitDirectBuffer",
        SubmitStatus::Internal.code(),
        || {
            // SAFETY: the sender's handle contract (module docs).
            let Some(producer) = (unsafe { producer(handle) }) else {
                return SubmitStatus::Invalid.code();
            };
            if buffer.is_null() {
                return SubmitStatus::Invalid.code();
            }
            let (Ok(address), Ok(capacity)) = (
                env.get_direct_buffer_address(&buffer),
                env.get_direct_buffer_capacity(&buffer),
            ) else {
                return SubmitStatus::Invalid.code();
            };
            let Some((offset, length)) = span(offset, length, capacity) else {
                return SubmitStatus::Invalid.code();
            };
            let data = if length == 0 || address.is_null() {
                &[][..]
            } else {
                // SAFETY: `address` addresses `capacity` bytes of a direct
                // buffer Kotlin keeps reachable for this call, and the span
                // lies within it. The bytes are copied before returning.
                unsafe { std::slice::from_raw_parts(address.add(offset), length) }
            };
            code(with_meta(&env, &meta, meta_len, |meta| {
                producer.submit_copy(meta, data)
            }))
        },
    )
}

/// Create the producer's pool of `count` Rust-owned slots of
/// `slot_size` bytes and return one direct `ByteBuffer` per slot, or
/// null when the shape is invalid. Called once per sender.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeOpenSlotPool<'local>(
    mut env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    slot_size: jint,
    count: jint,
) -> jobjectArray {
    let made = frame_guard("nativeOpenSlotPool", None, || {
        // SAFETY: the sender's handle contract (module docs).
        let producer = unsafe { producer(handle) }?;
        let size = usize::try_from(slot_size).ok()?;
        let count = u32::try_from(count).ok()?;
        let pool = producer.pool(size, count)?;
        let array = env
            .new_object_array(
                jint::try_from(count).ok()?,
                "java/nio/ByteBuffer",
                JObject::null(),
            )
            .ok()?;
        for slot in 0..count {
            // SAFETY: the slot spans `size` bytes of the pool, which
            // lives as long as the producer and its frames; Kotlin uses
            // the buffer only while it holds the slot and never after
            // freeing the producer (the sender drops its references).
            let view = unsafe { env.new_direct_byte_buffer(pool.slot_ptr(slot), size) }.ok()?;
            env.set_object_array_element(&array, jint::try_from(slot).ok()?, view)
                .ok()?;
        }
        Some(array.into_raw())
    });
    made.unwrap_or(std::ptr::null_mut())
}

/// A free slot for Kotlin to fill, or -1 when every slot is in use.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeAcquireSlot<'local>(
    _env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
) -> jint {
    frame_guard("nativeAcquireSlot", -1, || {
        // SAFETY: the sender's handle contract (module docs).
        unsafe { producer(handle) }
            .and_then(AndroidProducer::acquire_slot)
            .and_then(|slot| jint::try_from(slot).ok())
            .unwrap_or(-1)
    })
}

/// Give an acquired slot back unsubmitted.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeReleaseSlot<'local>(
    _env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    slot: jint,
) -> jboolean {
    let given = frame_guard("nativeReleaseSlot", false, || {
        // SAFETY: the sender's handle contract (module docs).
        let producer = unsafe { producer(handle) };
        let slot = u32::try_from(slot).ok();
        producer
            .zip(slot)
            .is_some_and(|(p, slot)| p.give_back_slot(slot))
    });
    if given { JNI_TRUE } else { JNI_FALSE }
}

/// Submit the first `length` bytes of an acquired slot, no copy.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeSubmitSlot<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    slot: jint,
    length: jint,
    meta: JByteArray<'local>,
    meta_len: jint,
) -> jint {
    frame_guard("nativeSubmitSlot", SubmitStatus::Internal.code(), || {
        // SAFETY: the sender's handle contract (module docs).
        let Some(producer) = (unsafe { producer(handle) }) else {
            return SubmitStatus::Invalid.code();
        };
        let (Ok(slot), Ok(length)) = (u32::try_from(slot), usize::try_from(length)) else {
            return SubmitStatus::Invalid.code();
        };
        code(with_meta(&env, &meta, meta_len, |meta| {
            producer.submit_slot(slot, length, meta)
        }))
    })
}

/// The tickets Rust released, waiting up to `timeout_ms` for the first
/// one; empty on timeout. Called from the sender's own releaser thread,
/// which closes each ticket's owner (`Image.close()`, …).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_runtime_IstmoFrames_nativeAwaitReleased<'local>(
    env: JNIEnv<'local>,
    _caller: JObject<'local>,
    handle: jlong,
    timeout_ms: jlong,
) -> jni::sys::jlongArray {
    let tickets = frame_guard("nativeAwaitReleased", Vec::new(), || {
        // SAFETY: the sender's handle contract (module docs).
        let Some(producer) = (unsafe { producer(handle) }) else {
            return Vec::new();
        };
        let timeout = Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
        producer.released().wait(timeout)
    });
    let Ok(len) = jint::try_from(tickets.len()) else {
        return std::ptr::null_mut();
    };
    env.new_long_array(len)
        .map_or(std::ptr::null_mut(), |array| {
            let _ = env.set_long_array_region(&array, 0, &tickets);
            array.into_raw()
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spans_stay_inside_the_buffer() {
        assert_eq!(span(0, 4, 4), Some((0, 4)));
        assert_eq!(span(2, 2, 4), Some((2, 2)));
        assert_eq!(span(3, 2, 4), None);
        assert_eq!(span(-1, 2, 4), None);
        assert_eq!(span(0, -1, 4), None);
        assert_eq!(
            span(i32::MAX, i32::MAX, usize::MAX),
            Some((i32::MAX as usize, i32::MAX as usize))
        );
    }

    #[test]
    fn a_null_handle_is_no_producer() {
        // SAFETY: 0 is the null handle.
        assert!(unsafe { producer(0) }.is_none());
    }
}
