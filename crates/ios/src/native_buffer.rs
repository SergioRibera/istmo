//! Platform buffers Swift hands to Rust frame sinks.
//!
//! Swift passes a retained object (`Unmanaged.passRetained(...).toOpaque()`)
//! and the function that releases it. Rust owns that +1 reference from
//! then on and calls the release function exactly once: when the frame
//! (and every clone of its buffer) drops, or at once when the submit is
//! refused. No Apple framework is linked from Rust; consumers reach the
//! object through [`RetainedBuffer::as_ptr`] (with `objc2-core-video`,
//! `objc2-io-surface`, …).

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use istmo_core::Runtime;
use istmo_core::native_frame::{MemoryKind, NativeBuffer, SubmitStatus};

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
            Self::CmSampleBuffer | Self::Other => MemoryKind::Other,
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
    /// The object (`CVPixelBufferRef`, `IOSurfaceRef`, …), valid while
    /// `self` lives.
    #[must_use]
    pub const fn as_ptr(&self) -> *mut c_void {
        self.object.as_ptr()
    }

    /// What the object is.
    #[must_use]
    pub const fn kind(&self) -> RetainedKind {
        self.kind
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

/// Submit the retained `object` to `sink` of `runtime` (`None`: no
/// runtime runs). `meta` is the wire-encoded metadata, `None` when its
/// buffer was malformed.
///
/// # Safety
///
/// When `object` and `release` are non-null, `object` must carry a +1
/// reference that `release` gives up; ownership passes to Rust whatever
/// the status. When either is null, nothing is taken.
pub(crate) unsafe fn submit_retained(
    runtime: Option<&Runtime>,
    sink: u64,
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
    let Some(meta) = meta else {
        return SubmitStatus::Invalid;
    };
    let Some(runtime) = runtime else {
        return SubmitStatus::NotStarted;
    };
    let memory = retained.kind.memory();
    runtime.submit_native_frame(sink, meta, NativeBuffer::new(memory, retained))
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
        let meta = codec::encode(&5u32).expect("encode");
        let before = RELEASED.load(Ordering::SeqCst);

        // SAFETY: `object()` stands for a retained object `count_release`
        // releases; nothing dereferences it.
        let status = unsafe {
            submit_retained(
                Some(&rt),
                sink.id().raw(),
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
        let retained = frame
            .buffer
            .downcast_ref::<RetainedBuffer>()
            .expect("retained");
        assert_eq!(retained.kind(), RetainedKind::CvPixelBuffer);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before);
        drop(frame);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 1);

        // Refused submits release at once.
        // SAFETY: as above.
        let status = unsafe {
            submit_retained(
                Some(&rt),
                u64::MAX,
                2,
                object(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::UnknownSink);
        // SAFETY: as above.
        let status = unsafe {
            submit_retained(
                None,
                sink.id().raw(),
                2,
                object(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::NotStarted);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 3);

        // Null object or release: nothing taken, nothing released.
        // SAFETY: null arguments are rejected before any use.
        let status = unsafe {
            submit_retained(
                Some(&rt),
                sink.id().raw(),
                1,
                std::ptr::null_mut(),
                Some(count_release),
                Some(&meta),
            )
        };
        assert_eq!(status, SubmitStatus::Invalid);
        // SAFETY: as above.
        let status =
            unsafe { submit_retained(Some(&rt), sink.id().raw(), 1, object(), None, Some(&meta)) };
        assert_eq!(status, SubmitStatus::Invalid);
        assert_eq!(RELEASED.load(Ordering::SeqCst), before + 3);
    }
}
