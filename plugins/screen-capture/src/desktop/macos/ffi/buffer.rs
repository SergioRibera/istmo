//! Sample buffers, their attachments, and the `IOSurface`-backed pixel
//! buffers inside them as frame backings: retained (no copy), mapped
//! read-only (no copy), or read back (a declared copy).

use std::ffi::c_void;
use std::fmt;
use std::ptr::NonNull;

use istmo_core::MemoryKind;
use istmo_core::native_frame::PlatformBuffer;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_core_foundation::{CFDictionary, CFRetained, CGRect};
use objc2_core_graphics::CGRectMakeWithDictionaryRepresentation;
use objc2_core_media::CMSampleBuffer;
use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetHeight, CVPixelBufferGetIOSurface,
    CVPixelBufferGetPixelFormatType, CVPixelBufferGetWidth,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSString, NSValue};
use objc2_io_surface::{IOSurfaceLockOptions, IOSurfaceRef};
use objc2_screen_capture_kit::{
    SCStreamFrameInfoContentRect, SCStreamFrameInfoDirtyRects, SCStreamFrameInfoScaleFactor,
    SCStreamFrameInfoStatus,
};

use crate::types::{CaptureError, PlaneLayout, Size};

use super::super::super::sck_map::{self, Attachments, FourCc, ID, RectF};

/// A retained `CMSampleBuffer` from a stream or a screenshot.
pub(crate) struct Sample(pub(super) CFRetained<CMSampleBuffer>);

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "Core Foundation objects with thread-safe retain / release; see the SAFETY note"
)]
// SAFETY: Core Foundation retain / release is thread-safe, and a
// sample buffer is immutable once the framework handed it out; the
// plugin only reads it.
unsafe impl Send for Sample {}

impl fmt::Debug for Sample {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Sample").finish_non_exhaustive()
    }
}

impl Sample {
    /// Retain `sample` past the callback that lent it.
    pub(super) fn retain(sample: &CMSampleBuffer) -> Self {
        // SAFETY: `sample` is a valid object for the callback's duration;
        // retaining it gives this value its own reference.
        Self(unsafe { CFRetained::retain(NonNull::from(sample)) })
    }

    /// Whether the buffer holds valid data.
    #[must_use]
    pub(crate) fn is_valid(&self) -> bool {
        // SAFETY: a getter on a valid sample buffer.
        let valid = unsafe { self.0.is_valid() };
        // SAFETY: as above.
        valid && unsafe { self.0.data_is_ready() }
    }

    /// The presentation time on the host time clock, in nanoseconds.
    #[must_use]
    pub(crate) fn pts_ns(&self) -> Option<u64> {
        // SAFETY: a getter on a valid sample buffer.
        let time = unsafe { self.0.presentation_time_stamp() };
        sck_map::cmtime_ns(time.value, time.timescale)
    }

    /// The first sample's attachments (status, content rect, scale,
    /// dirty rects); empty when there are none.
    #[must_use]
    pub(crate) fn attachments(&self) -> Attachments {
        // SAFETY: a getter on a valid sample buffer; `false` never creates
        // the array.
        let Some(array) = (unsafe { self.0.sample_attachments_array(false) }) else {
            return Attachments::default();
        };
        // SAFETY: CFArray / CFDictionary are toll-free bridged with
        // NSArray / NSDictionary; the array (retained above) outlives the
        // borrow, and SCK documents its entries as dictionaries.
        let array: &NSArray<NSDictionary<NSString, AnyObject>> =
            unsafe { &*CFRetained::as_ptr(&array).as_ptr().cast() };
        if array.count() == 0 {
            return Attachments::default();
        }
        let info = array.objectAtIndex(0);
        // SAFETY: the keys are immutable framework constants.
        let (status, rect, scale, dirty) = unsafe {
            (
                SCStreamFrameInfoStatus,
                SCStreamFrameInfoContentRect,
                SCStreamFrameInfoScaleFactor,
                SCStreamFrameInfoDirtyRects,
            )
        };
        let number = |key: &NSString| -> Option<Retained<NSNumber>> {
            info.objectForKey(key)
                .and_then(|v| v.downcast_ref::<NSNumber>().map(objc2::Message::retain))
        };
        Attachments {
            status: number(status).map(|n| i64::try_from(n.integerValue()).unwrap_or(i64::MIN)),
            content_rect: info.objectForKey(rect).as_deref().and_then(rect_of),
            scale_factor: number(scale).map(|n| n.doubleValue()),
            dirty_rects: info.objectForKey(dirty).map(|value| {
                value
                    .downcast_ref::<NSArray>()
                    .map(|rects| rects.iter().filter_map(|r| rect_of(&r)).collect())
                    .unwrap_or_default()
            }),
        }
    }

    /// The pixel buffer, when the sample carries one backed by an
    /// `IOSurface` (every SCK video frame does; idle frames carry none).
    #[must_use]
    pub(crate) fn pixel_buffer(&self) -> Option<PixelBuffer> {
        // SAFETY: a getter on a valid sample buffer; the binding retains
        // the result.
        let buffer = unsafe { self.0.image_buffer() }?;
        let surface = CVPixelBufferGetIOSurface(Some(&buffer))?;
        Some(PixelBuffer { buffer, surface })
    }
}

/// A rectangle stored as an `NSValue` or as a `CGRect` dictionary.
fn rect_of(value: &AnyObject) -> Option<RectF> {
    if let Some(value) = value.downcast_ref::<NSValue>() {
        let rect = value.get_rect()?;
        return Some(RectF {
            x: rect.origin.x,
            y: rect.origin.y,
            width: rect.size.width,
            height: rect.size.height,
        });
    }
    let dictionary = value.downcast_ref::<NSDictionary>()?;
    let mut rect = CGRect::default();
    // SAFETY: NSDictionary and CFDictionary are toll-free bridged, and the
    // borrow keeps the object alive.
    let dictionary = unsafe { &*std::ptr::from_ref(dictionary).cast::<CFDictionary>() };
    // SAFETY: a valid dictionary; `rect` is a valid out-pointer.
    let parsed = unsafe { CGRectMakeWithDictionaryRepresentation(Some(dictionary), &raw mut rect) };
    parsed.then_some(RectF {
        x: rect.origin.x,
        y: rect.origin.y,
        width: rect.size.width,
        height: rect.size.height,
    })
}

/// An `IOSurface`-backed `CVPixelBuffer`, retained.
pub(crate) struct PixelBuffer {
    buffer: CFRetained<CVPixelBuffer>,
    surface: CFRetained<IOSurfaceRef>,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "Core Foundation objects with thread-safe retain / release; see the SAFETY note"
)]
// SAFETY: retain / release of Core Video and IOSurface objects is
// thread-safe; while retained, the stream's pool does not recycle the
// surface, so nobody writes into it.
unsafe impl Send for PixelBuffer {}

impl fmt::Debug for PixelBuffer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PixelBuffer")
            .field("surface", &self.surface_id())
            .field("size", &self.size())
            .finish_non_exhaustive()
    }
}

impl PixelBuffer {
    #[must_use]
    pub(crate) fn size(&self) -> Size {
        let width = u32::try_from(CVPixelBufferGetWidth(&self.buffer)).unwrap_or(0);
        let height = u32::try_from(CVPixelBufferGetHeight(&self.buffer)).unwrap_or(0);
        Size::new(width, height)
    }

    #[must_use]
    pub(crate) fn fourcc(&self) -> FourCc {
        FourCc(CVPixelBufferGetPixelFormatType(&self.buffer))
    }

    /// The surface's process-wide id: the same id is the same memory.
    #[must_use]
    pub(crate) fn surface_id(&self) -> u32 {
        self.surface.id()
    }

    /// Plane offsets (from the surface's base address), strides and
    /// sizes.
    #[must_use]
    pub(crate) fn planes(&self) -> Vec<PlaneLayout> {
        layout(&self.surface)
    }

    /// Hand the buffer over as `kind` (`IoSurface` or `CvPixelBuffer`),
    /// retained: no copy.
    #[must_use]
    pub(crate) fn into_frame(self, kind: MemoryKind) -> IoSurfaceFrame {
        IoSurfaceFrame {
            buffer: self.buffer,
            surface: self.surface,
            kind,
        }
    }

    /// Lock the surface read-only and hand its memory over: no copy.
    ///
    /// # Errors
    /// `IOSurfaceLock` failed.
    pub(crate) fn map(self) -> Result<CpuSurface, CaptureError> {
        lock(&self.surface, "map")?;
        let base = self.surface.base_address().cast::<u8>();
        let len = self.surface.alloc_size();
        Ok(CpuSurface {
            _buffer: self.buffer,
            surface: self.surface,
            base,
            len,
        })
    }

    /// Copy the surface's memory out (`CopyStage::ExhaustionReadback`):
    /// the bytes and their plane layout, the surface released at once.
    ///
    /// # Errors
    /// `IOSurfaceLock` failed.
    pub(crate) fn read(&self) -> Result<Vec<u8>, CaptureError> {
        lock(&self.surface, "readback")?;
        let len = self.surface.alloc_size();
        let base = self.surface.base_address().cast::<u8>();
        // SAFETY: the surface is locked read-only and `alloc_size` bytes
        // from its base address are its memory.
        let bytes = unsafe { std::slice::from_raw_parts(base.as_ptr(), len) }.to_vec();
        unlock(&self.surface);
        Ok(bytes)
    }
}

fn layout(surface: &IOSurfaceRef) -> Vec<PlaneLayout> {
    let base = surface.base_address().as_ptr() as usize;
    let plane = |offset: usize, stride: usize, height: usize| PlaneLayout {
        offset: u32::try_from(offset).unwrap_or(u32::MAX),
        stride: u32::try_from(stride).unwrap_or(u32::MAX),
        size: u64::try_from(stride.saturating_mul(height)).unwrap_or(u64::MAX),
    };
    match surface.plane_count() {
        0 => vec![plane(0, surface.bytes_per_row(), surface.height())],
        count => (0..count)
            .map(|index| {
                let start = surface.base_address_of_plane(index).as_ptr() as usize;
                plane(
                    start.saturating_sub(base),
                    surface.bytes_per_row_of_plane(index),
                    surface.height_of_plane(index),
                )
            })
            .collect(),
    }
}

fn lock(surface: &IOSurfaceRef, operation: &str) -> Result<(), CaptureError> {
    // SAFETY: a read-only lock of a retained surface; a null seed is
    // allowed.
    let status = unsafe { surface.lock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    if status == 0 {
        Ok(())
    } else {
        Err(CaptureError::os(ID, operation, i64::from(status)))
    }
}

fn unlock(surface: &IOSurfaceRef) {
    // SAFETY: balances the read-only lock taken by `lock`.
    let status = unsafe { surface.unlock(IOSurfaceLockOptions::ReadOnly, std::ptr::null_mut()) };
    if status != 0 {
        tracing::warn!(status, "screen-capture: IOSurfaceUnlock failed");
    }
}

/// A macOS frame backing: the retained `IOSurface` of a ScreenCaptureKit
/// frame and the `CVPixelBuffer` around it, handed over without a copy.
///
/// [`NativeBuffer::kind`](istmo_core::NativeBuffer::kind) says which of
/// the two [`platform_ptr`](istmo_core::NativeBuffer::platform_ptr)
/// points at (what the session's `BufferMemory` asked for); both are
/// reachable here, through `NativeBuffer::downcast_ref::<IoSurfaceFrame>()`.
/// While it lives, ScreenCaptureKit does not reuse the surface.
pub struct IoSurfaceFrame {
    buffer: CFRetained<CVPixelBuffer>,
    surface: CFRetained<IOSurfaceRef>,
    kind: MemoryKind,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "Core Foundation objects with thread-safe retain / release; see the SAFETY note"
)]
// SAFETY: retain / release of Core Video and IOSurface objects is
// thread-safe and this value owns one reference to each; reading the
// pixels needs the platform's own locking (`IOSurfaceLock`,
// `CVPixelBufferLockBaseAddress`), which is the consumer's business.
unsafe impl Send for IoSurfaceFrame {}
// SAFETY: as above; `&IoSurfaceFrame` only hands out pointers and ids.
unsafe impl Sync for IoSurfaceFrame {}

impl IoSurfaceFrame {
    /// The `CVPixelBufferRef`, valid while `self` lives.
    #[must_use]
    pub fn pixel_buffer_ptr(&self) -> NonNull<c_void> {
        CFRetained::as_ptr(&self.buffer).cast()
    }

    /// The `IOSurfaceRef`, valid while `self` lives.
    #[must_use]
    pub fn io_surface_ptr(&self) -> NonNull<c_void> {
        CFRetained::as_ptr(&self.surface).cast()
    }

    /// The surface's process-wide id (`IOSurfaceGetID`).
    #[must_use]
    pub fn surface_id(&self) -> u32 {
        self.surface.id()
    }
}

impl PlatformBuffer for IoSurfaceFrame {
    fn kind(&self) -> MemoryKind {
        self.kind
    }

    fn as_ptr(&self) -> NonNull<c_void> {
        if self.kind == MemoryKind::CvPixelBuffer {
            self.pixel_buffer_ptr()
        } else {
            self.io_surface_ptr()
        }
    }
}

impl fmt::Debug for IoSurfaceFrame {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("IoSurfaceFrame")
            .field("surface", &self.surface_id())
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

/// A surface locked read-only and lent out as bytes: CPU frames without
/// a copy. Unlocked (and released) when the last frame clone drops.
pub(crate) struct CpuSurface {
    _buffer: CFRetained<CVPixelBuffer>,
    surface: CFRetained<IOSurfaceRef>,
    base: NonNull<u8>,
    len: usize,
}

#[expect(
    clippy::non_send_fields_in_send_ty,
    reason = "Core Foundation objects with thread-safe retain / release; see the SAFETY note"
)]
// SAFETY: the memory is locked read-only and nobody writes it while the
// surface is retained (the stream's pool skips retained surfaces);
// retain / release / unlock are thread-safe.
unsafe impl Send for CpuSurface {}
// SAFETY: as above; shared access only reads.
unsafe impl Sync for CpuSurface {}

impl AsRef<[u8]> for CpuSurface {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: `len` bytes from `base` are the locked surface's memory,
        // valid until `drop` unlocks it.
        unsafe { std::slice::from_raw_parts(self.base.as_ptr(), self.len) }
    }
}

impl Drop for CpuSurface {
    fn drop(&mut self) {
        unlock(&self.surface);
    }
}
