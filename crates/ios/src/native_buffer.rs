//! Frame sinks fed from Swift: the C entries `IstmoFrameSender` calls,
//! and the retained Core Foundation objects it hands over.
//!
//! A sender opens one producer (`istmo_ios_open_frame_producer`, an
//! opaque pointer) and submits through it: no registry lookup and no
//! `Runtime::global()` per frame. Swift passes a retained object
//! (`Unmanaged.passRetained(...).toOpaque()`) and the function that
//! releases it; Rust owns that +1 reference from then on and calls the
//! release function exactly once — when the frame (and every clone of
//! its buffer) drops, or at once when the submit is refused. A retained
//! pixel buffer is not recycled by its pool while Rust holds it, so the
//! producer never writes into a frame a consumer still reads. No Apple
//! framework is linked from Rust; consumers reach the object through
//! [`NativeBuffer::platform_ptr`] (with `objc2-core-video`,
//! `objc2-io-surface`, …).
//!
//! Every entry answers a `SubmitStatus` code and never unwinds into
//! Swift: a panic answers `INTERNAL`.

use std::ffi::c_void;
use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr::NonNull;

use istmo_core::Runtime;
use istmo_core::native_frame::{
    BytePool, MemoryKind, NATIVE_ABI_VERSION, NativeBuffer, NativeProducer, PlatformBuffer,
    SubmitStatus,
};

/// Releases the +1 reference Swift passed (`CFRelease`, or
/// `Unmanaged.fromOpaque(..).release()` behind a C function pointer).
pub type ReleaseFn = unsafe extern "C" fn(object: *mut c_void);

/// What a [`RetainedBuffer`] points at; the `kind` argument of
/// `istmo_ios_submit_native_buffer`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum RetainedKind {
    /// Anything else (`kind` 0, or a tag this build does not know).
    Other,
    /// `CVPixelBufferRef` (`kind` 1).
    CvPixelBuffer,
    /// `IOSurfaceRef` (`kind` 2).
    IoSurface,
    /// `CMSampleBufferRef` (`kind` 3).
    CmSampleBuffer,
}

impl RetainedKind {
    /// Every kind with its C tag, for the parity tests.
    pub const ALL: [(Self, u32); 4] = [
        (Self::Other, 0),
        (Self::CvPixelBuffer, 1),
        (Self::IoSurface, 2),
        (Self::CmSampleBuffer, 3),
    ];

    /// The kind for the C tag `kind`.
    #[must_use]
    pub const fn from_tag(kind: u32) -> Self {
        match kind {
            1 => Self::CvPixelBuffer,
            2 => Self::IoSurface,
            3 => Self::CmSampleBuffer,
            _ => Self::Other,
        }
    }

    const fn memory(self) -> MemoryKind {
        match self {
            Self::CvPixelBuffer => MemoryKind::CvPixelBuffer,
            Self::IoSurface => MemoryKind::IoSurface,
            Self::CmSampleBuffer => MemoryKind::CmSampleBuffer,
            Self::Other => MemoryKind::Other,
        }
    }
}

/// A Core Foundation object Rust holds one reference to.
pub struct RetainedBuffer {
    object: NonNull<c_void>,
    kind: RetainedKind,
    release: ReleaseFn,
}

// SAFETY: retain / release of Core Foundation objects (CVPixelBuffer,
// IOSurface, CMSampleBuffer) is thread-safe, and this value owns exactly
// one reference. Reading the pixels needs the platform's own locking
// (`CVPixelBufferLockBaseAddress`), which is the consumer's business.
unsafe impl Send for RetainedBuffer {}
// SAFETY: as above; `&RetainedBuffer` only hands out the pointer.
unsafe impl Sync for RetainedBuffer {}

impl RetainedBuffer {
    /// What the object is.
    #[must_use]
    pub const fn retained_kind(&self) -> RetainedKind {
        self.kind
    }
}

impl PlatformBuffer for RetainedBuffer {
    fn kind(&self) -> MemoryKind {
        self.kind.memory()
    }

    fn as_ptr(&self) -> NonNull<c_void> {
        self.object
    }
}

impl Drop for RetainedBuffer {
    fn drop(&mut self) {
        // SAFETY: `object` carries the one reference this value owns;
        // `release` is the function Swift paired with it.
        unsafe { (self.release)(self.object.as_ptr()) };
    }
}

impl fmt::Debug for RetainedBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RetainedBuffer")
            .field("object", &self.object)
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// `(ptr, len)` as a slice, without copying.
///
/// # Safety
///
/// When `len > 0`, `ptr` addresses `len` readable bytes for the duration
/// of the call.
unsafe fn borrow<'a>(ptr: *const u8, len: usize) -> Option<&'a [u8]> {
    if len == 0 {
        return Some(&[]);
    }
    if ptr.is_null() || isize::try_from(len).is_err() {
        return None;
    }
    // SAFETY: non-null, at most `isize::MAX` bytes (checked above), and
    // readable for the call per this function's contract.
    Some(unsafe { std::slice::from_raw_parts(ptr, len) })
}

/// Run a frame entry, answering `on_panic` instead of unwinding.
fn guard<R>(export: &'static str, on_panic: R, body: impl FnOnce() -> R) -> R {
    catch_unwind(AssertUnwindSafe(body)).unwrap_or_else(|_| {
        tracing::error!(export, "istmo frame entry panicked");
        on_panic
    })
}

/// Opaque producer handle Swift's `IstmoFrameSender` holds.
#[derive(Debug)]
pub struct IstmoFrameProducer(NativeProducer, BytePool);

/// Submit the retained `object` through `producer` (`None`: a null
/// handle). `meta` is `None` when its buffer was malformed.
///
/// # Safety
///
/// When `object` and `release` are non-null, `object` must carry a +1
/// reference that `release` gives up; ownership passes to Rust whatever
/// the status. When either is null, nothing is taken.
pub(crate) unsafe fn submit_retained(
    producer: Option<&NativeProducer>,
    kind: u32,
    object: *mut c_void,
    release: Option<ReleaseFn>,
    meta: Option<&[u8]>,
) -> SubmitStatus {
    let (Some(object), Some(release)) = (NonNull::new(object), release) else {
        return SubmitStatus::Invalid;
    };
    // Owned from here on: every early return below releases it.
    let retained = RetainedBuffer {
        object,
        kind: RetainedKind::from_tag(kind),
        release,
    };
    let (Some(producer), Some(meta)) = (producer, meta) else {
        return SubmitStatus::Invalid;
    };
    producer.submit(meta, NativeBuffer::from_platform(retained))
}

/// The native entry version; `IstmoRuntime.start()` refuses a library
/// whose version differs from the one it was built for.
#[unsafe(no_mangle)]
pub const extern "C" fn istmo_abi_version() -> u32 {
    NATIVE_ABI_VERSION
}

/// Open a producer on sink `sink` presenting metadata `fingerprint`
/// (0: unchecked). Returns the handle, or null with the reason in
/// `*status` (when `status` is non-null).
///
/// # Safety
///
/// `status` is null or valid for one `int32_t` write.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_open_frame_producer(
    sink: u64,
    fingerprint: u64,
    status: *mut i32,
) -> *mut IstmoFrameProducer {
    let opened = guard(
        "istmo_ios_open_frame_producer",
        Err(SubmitStatus::Internal),
        || {
            let runtime = Runtime::global().map_err(|_| SubmitStatus::NotStarted)?;
            runtime.open_native_producer(sink, fingerprint)
        },
    );
    let (handle, outcome) = match opened {
        Ok(producer) => (
            Box::into_raw(Box::new(IstmoFrameProducer(producer, BytePool::default()))),
            SubmitStatus::Accepted,
        ),
        Err(status) => (std::ptr::null_mut(), status),
    };
    if !status.is_null() {
        // SAFETY: non-null, and valid for a write per the contract.
        unsafe { status.write(outcome.code()) };
    }
    handle
}

/// End the stream: completed when `error` is null, failed with the
/// UTF-8 text `(error, error_len)` otherwise.
///
/// # Safety
///
/// `producer` is null or a live handle; `(error, error_len)` is null or
/// readable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_end_frame_producer(
    producer: *const IstmoFrameProducer,
    error: *const u8,
    error_len: usize,
) {
    guard("istmo_ios_end_frame_producer", (), || {
        // SAFETY: forwarded from this function's contract.
        let Some(producer) = (unsafe { producer.as_ref() }) else {
            return;
        };
        if error.is_null() {
            producer.0.close();
        } else {
            // SAFETY: forwarded from this function's contract.
            let text = unsafe { borrow(error, error_len) }.unwrap_or_default();
            producer.0.close_with_message(String::from_utf8_lossy(text));
        }
    });
}

/// Free a producer handle; it no longer counts as a producer of its
/// sink. Frames already submitted keep their buffers.
///
/// # Safety
///
/// `producer` is null or a live handle, freed once and not used after.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_free_frame_producer(producer: *mut IstmoFrameProducer) {
    guard("istmo_ios_free_frame_producer", (), || {
        if !producer.is_null() {
            // SAFETY: forwarded from this function's contract.
            drop(unsafe { Box::from_raw(producer) });
        }
    });
}

/// Hand a retained Core Foundation object to the producer's sink.
///
/// `kind`: 1 `CVPixelBufferRef`, 2 `IOSurfaceRef`, 3 `CMSampleBufferRef`,
/// 0 anything else. The metadata is wire-encoded in `(meta, meta_len)`,
/// read in place.
///
/// Returns a `SubmitStatus` code. Rust takes over the +1 reference in
/// `object` whatever the status (releasing it at once when the frame is
/// refused), unless `object` or `release` is null: then the status is
/// `INVALID` (7) and the caller keeps it.
///
/// # Safety
///
/// `producer` is null or a live handle; `object` carries a +1 reference
/// that `release` gives up, and `release` may run on any thread;
/// `(meta, meta_len)` is readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_native_buffer(
    producer: *const IstmoFrameProducer,
    kind: u32,
    object: *mut c_void,
    release: Option<ReleaseFn>,
    meta: *const u8,
    meta_len: usize,
) -> i32 {
    guard(
        "istmo_ios_submit_native_buffer",
        SubmitStatus::Internal.code(),
        || {
            // SAFETY: forwarded from this function's contract.
            let (producer, meta) = unsafe { (producer.as_ref(), borrow(meta, meta_len)) };
            // SAFETY: forwarded from this function's contract.
            unsafe { submit_retained(producer.map(|p| &p.0), kind, object, release, meta) }.code()
        },
    )
}

/// Copy `(bytes, len)` into a frame for the producer's sink. Returns a
/// `SubmitStatus` code.
///
/// # Safety
///
/// `producer` is null or a live handle; both `(ptr, len)` pairs are
/// readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn istmo_ios_submit_frame_bytes(
    producer: *const IstmoFrameProducer,
    bytes: *const u8,
    len: usize,
    meta: *const u8,
    meta_len: usize,
) -> i32 {
    guard(
        "istmo_ios_submit_frame_bytes",
        SubmitStatus::Internal.code(),
        || {
            // SAFETY: forwarded from this function's contract.
            let parts = unsafe {
                (
                    producer.as_ref(),
                    borrow(bytes, len),
                    borrow(meta, meta_len),
                )
            };
            let (Some(producer), Some(data), Some(meta)) = parts else {
                return SubmitStatus::Invalid.code();
            };
            producer.0.submit(meta, producer.1.copy_of(data)).code()
        },
    )
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use istmo_core::codec;
    use istmo_core::native_frame::SinkPolicy;

    use super::*;

    static RELEASED: AtomicUsize = AtomicUsize::new(0);

    unsafe extern "C" fn count_release(object: *mut c_void) {
        assert!(!object.is_null());
        RELEASED.fetch_add(1, Ordering::SeqCst);
    }

    fn object() -> *mut c_void {
        NonNull::<c_void>::dangling().as_ptr()
    }

    #[test]
    fn ownership_passes_to_rust_and_is_released_once() {
        let rt = Runtime::mock().runtime;
        let sink = rt
            .open_frame_sink::<u32>(SinkPolicy::latest())
            .expect("open");
        let producer = rt
            .open_native_producer(sink.id().raw(), 0)
            .expect("producer");
        let meta = codec::encode(&5u32).expect("encode");
        let before = RELEASED.load(Ordering::SeqCst);

        // SAFETY: `object()` stands for a retained object `count_release`
        // releases; nothing dereferences it.
        let status = unsafe {
            submit_retained(
                Some(&producer),
                1,
                object(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::Accepted);
        let frame = sink.try_recv().expect("frame");
        assert_eq!(frame.meta, 5);
        assert_eq!(frame.buffer.kind(), MemoryKind::CvPixelBuffer);
        assert_eq!(
            frame.buffer.platform_ptr().map(NonNull::as_ptr),
            Some(object())
        );
        let retained = frame
            .buffer
            .downcast_ref::<RetainedBuffer>()
            .expect("retained");
        assert_eq!(retained.retained_kind(), RetainedKind::CvPixelBuffer);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before);
        drop(frame);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 1);

        // Refused submits release at once: bad metadata, a null producer,
        // a malformed metadata buffer.
        // SAFETY: as above.
        let status = unsafe {
            submit_retained(
                Some(&producer),
                3,
                object(),
                Some(count_release),
                Some(&[0xff]),
            )
        };
        assert_eq!(status, SubmitStatus::BadMeta);
        // SAFETY: as above.
        let status =
            unsafe { submit_retained(None, 2, object(), Some(count_release), Some(&meta)) };
        assert_eq!(status, SubmitStatus::Invalid);
        // SAFETY: as above.
        let status =
            unsafe { submit_retained(Some(&producer), 2, object(), Some(count_release), None) };
        assert_eq!(status, SubmitStatus::Invalid);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 4);

        // Null object or release: nothing taken, nothing released.
        // SAFETY: null arguments are rejected before any use.
        let status = unsafe {
            submit_retained(
                Some(&producer),
                1,
                std::ptr::null_mut(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::Invalid);
        // SAFETY: as above.
        let status = unsafe { submit_retained(Some(&producer), 1, object(), None, Some(&meta)) };
        assert_eq!(status, SubmitStatus::Invalid);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 4);

        // A closed sink releases too.
        producer.close();
        // SAFETY: as above.
        let status = unsafe {
            submit_retained(
                Some(&producer),
                1,
                object(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::Closed);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 5);
    }

    #[test]
    fn c_entries_without_a_runtime_or_with_bad_input() {
        assert_eq!(istmo_abi_version(), NATIVE_ABI_VERSION);
        let mut status = -1;
        // SAFETY: `status` is a valid `i32`.
        let handle = unsafe { istmo_ios_open_frame_producer(1, 0, &raw mut status) };
        assert!(handle.is_null());
        assert_eq!(status, SubmitStatus::NotStarted.code());
        let meta = [0u8; 1];
        // SAFETY: null handle, readable buffers.
        let code = unsafe {
            istmo_ios_submit_frame_bytes(std::ptr::null(), meta.as_ptr(), 1, meta.as_ptr(), 1)
        };
        assert_eq!(code, SubmitStatus::Invalid.code());
        // SAFETY: a null handle is ignored.
        unsafe {
            istmo_ios_end_frame_producer(std::ptr::null(), std::ptr::null(), 0);
            istmo_ios_free_frame_producer(std::ptr::null_mut());
        }
        // SAFETY: a length beyond `isize::MAX` is refused before any read.
        assert!(unsafe { borrow(meta.as_ptr(), usize::MAX) }.is_none());
        // SAFETY: null with a length is refused.
        assert!(unsafe { borrow(std::ptr::null(), 4) }.is_none());
    }

    #[test]
    fn handles_feed_their_sink_and_end_it() {
        let rt = Runtime::mock().runtime;
        let sink = rt
            .open_frame_sink::<u32>(SinkPolicy::queue(4))
            .expect("open");
        let handle = Box::into_raw(Box::new(IstmoFrameProducer(
            rt.open_native_producer(
                sink.id().raw(),
                istmo_core::native_frame::meta_fingerprint::<u32>(),
            )
            .expect("producer"),
            BytePool::default(),
        )));
        let meta = codec::encode(&9u32).expect("meta");
        let data = [1u8, 2, 3];
        // SAFETY: a live handle and readable buffers.
        let code = unsafe {
            istmo_ios_submit_frame_bytes(
                handle,
                data.as_ptr(),
                data.len(),
                meta.as_ptr(),
                meta.len(),
            )
        };
        assert_eq!(code, SubmitStatus::Accepted.code());
        let error = b"camera gone";
        // SAFETY: a live handle and a readable message; freed once.
        unsafe {
            istmo_ios_end_frame_producer(handle, error.as_ptr(), error.len());
            istmo_ios_free_frame_producer(handle);
        }
        let frame = sink.try_recv().expect("frame");
        assert_eq!((frame.meta, frame.buffer.as_bytes()), (9, Some(&data[..])));
        assert_eq!(
            sink.end_reason()
                .and_then(|r| r.error().map(ToString::to_string))
                .as_deref(),
            Some("camera gone")
        );
    }
}
