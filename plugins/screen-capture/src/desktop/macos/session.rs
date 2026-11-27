//! Sample buffers into frames: the live sink of a stream (pool holds,
//! exhaustion, status events) and one-shot captures for screenshots.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use istmo_core::{MemoryKind, NativeBuffer};

use crate::pool::{HoldError, PoolAccounting};
use crate::types::{
    BufferIdentity, BufferMemory, CaptureError, CaptureEvent, Colour, CopyStage, Exhaustion,
    FrameMeta, PauseReason, PixelFormat, PtsClock, Size, SourceId, SyncKind,
};

use super::super::output::{Delivery, FrameOut};
use super::super::sck_map::{self, FrameStatus, ID, SlotChange, SlotMap};
use super::ffi::{Filter, Kind, PixelBuffer, Sample, Settings, Sink, Stream};

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    // Short state updates only; a poisoned lock still holds valid state.
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// How long a one-frame stream (macOS 12.3–13 screenshots) may take to
/// deliver its frame.
const ONE_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// What every frame of a session (or a screenshot) shares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Shape {
    pub(super) format: PixelFormat,
    pub(super) memory: BufferMemory,
    pub(super) colour: Colour,
    pub(super) tier: u8,
    pub(super) source_id: SourceId,
}

impl Shape {
    /// The kind of a retained (not mapped) frame: `CvPixelBuffer` when
    /// asked for, the `IOSurface` otherwise.
    const fn memory_kind(&self) -> MemoryKind {
        match self.memory {
            BufferMemory::CvPixelBuffer => MemoryKind::CvPixelBuffer,
            BufferMemory::IoSurface
            | BufferMemory::Cpu
            | BufferMemory::DmaBuf
            | BufferMemory::D3d11Texture
            | BufferMemory::HardwareBuffer
            | BufferMemory::WebVideoFrame => MemoryKind::IoSurface,
        }
    }
}

/// The frame's metadata, from its sample and pixel buffer.
fn meta_for(shape: &Shape, sample: &Sample, pixels: &PixelBuffer, pts_ns: u64) -> FrameMeta {
    let size = pixels.size();
    let attachments = sample.attachments();
    // What SCK delivered, should it differ from what was asked.
    let (format, expected) = match pixels.fourcc().format() {
        Some((format, range)) if format != shape.format => {
            (format, sck_map::colour(format, range, false))
        }
        _ => (shape.format, shape.colour),
    };
    // The buffer's own colour tags say what was captured.
    let colour = pixels.colour(expected);
    FrameMeta {
        seq: 0,
        pts_ns,
        pts_clock: PtsClock::HostTime,
        wall_us: 0,
        size,
        content_rect: attachments.content_rect(size),
        format,
        memory: shape.memory,
        planes: pixels.planes(),
        modifier: None,
        buffer: BufferIdentity::default(),
        tier: shape.tier,
        copied: None,
        producer: None,
        colour,
        hdr: None,
        // The window server composes displays upright.
        orientation: crate::types::Orientation::UPRIGHT,
        damage: attachments.damage(size),
        cursor: None,
        sync: SyncKind::Ready,
        scale_permille: attachments.scale_permille(),
        source_id: shape.source_id.clone(),
        repeat: false,
        skipped_before: 0,
        protected_content: false,
        composite: None,
    }
}

/// `pixels` as a backing in `shape.memory`, without a copy.
fn backing(shape: &Shape, pixels: PixelBuffer) -> Result<NativeBuffer, CaptureError> {
    if shape.memory == BufferMemory::Cpu {
        Ok(NativeBuffer::from_bytes(pixels.map()?))
    } else {
        Ok(NativeBuffer::from_platform(
            pixels.into_frame(shape.memory_kind()),
        ))
    }
}

/// One frame of a finished one-shot capture.
pub(super) fn still(
    shape: &Shape,
    sample: &Sample,
) -> Result<(FrameMeta, NativeBuffer), CaptureError> {
    let pixels = sample
        .pixel_buffer()
        .ok_or_else(|| CaptureError::protocol(ID, "screenshot", "no pixel buffer"))?;
    let pts = sample
        .pts_ns()
        .or_else(super::ffi::host_time_ns)
        .unwrap_or_default();
    let meta = meta_for(shape, sample, &pixels, pts);
    let buffer = backing(shape, pixels)?;
    Ok((meta, buffer))
}

/// Takes the first complete frame of a stream.
struct OneFrame {
    tx: flume::Sender<Result<Sample, CaptureError>>,
}

impl Sink for OneFrame {
    fn sample(&self, kind: Kind, sample: Sample) {
        if kind == Kind::Screen
            && sample.attachments().status() == FrameStatus::Complete
            && sample.pixel_buffer().is_some()
        {
            let _sent = self.tx.try_send(Ok(sample));
        }
    }

    fn stopped(&self, error: CaptureError) {
        let _sent = self.tx.try_send(Err(error));
    }
}

/// One frame through a short-lived stream: the screenshot path before
/// `SCScreenshotManager` (macOS 12.3–13).
pub(super) fn one_frame(filter: &Filter, settings: &Settings) -> Result<Sample, CaptureError> {
    let (tx, rx) = flume::bounded(1);
    let stream = Stream::start(filter, settings, true, false, Arc::new(OneFrame { tx }))?;
    let sample = rx
        .recv_timeout(ONE_FRAME_TIMEOUT)
        .unwrap_or_else(|_| Err(CaptureError::protocol(ID, "screenshot", "no frame arrived")));
    drop(stream);
    sample
}

#[derive(Debug)]
struct LiveState {
    shape: Shape,
    slots: SlotMap,
    /// Frames released unseen since the last delivered one.
    skipped: u32,
    suspended: bool,
    size: Option<Size>,
}

/// A live stream's sink: holds each delivered surface in the pool
/// accounting (never the last free one), applies the exhaustion policy
/// and reports suspension and resizes.
#[derive(Debug)]
pub(super) struct LiveSink {
    out: FrameOut,
    pool: PoolAccounting,
    depth: u8,
    exhaustion: Exhaustion,
    state: Mutex<LiveState>,
}

impl LiveSink {
    pub(super) fn new(out: FrameOut, shape: Shape, depth: u8, exhaustion: Exhaustion) -> Self {
        let pool = PoolAccounting::new(usize::from(depth));
        // A generation of its own, so slots know the pool's.
        let generation = pool.reallocate(usize::from(depth));
        let slots = SlotMap::new(generation);
        Self {
            out,
            pool,
            depth,
            exhaustion,
            state: Mutex::new(LiveState {
                shape,
                slots,
                skipped: 0,
                suspended: false,
                size: None,
            }),
        }
    }

    /// New formats: frames from here on carry `shape`, in a new pool
    /// generation.
    pub(super) fn reshape(&self, shape: Shape) {
        let mut state = lock(&self.state);
        state.shape = shape;
        let generation = self.pool.reallocate(usize::from(self.depth));
        state.slots.restart(generation);
    }

    fn status(&self, status: FrameStatus) -> bool {
        let mut state = lock(&self.state);
        match status {
            FrameStatus::Complete => {
                if std::mem::take(&mut state.suspended) {
                    drop(state);
                    self.out.event(CaptureEvent::Resumed);
                }
                true
            }
            FrameStatus::Suspended => {
                if !std::mem::replace(&mut state.suspended, true) {
                    drop(state);
                    self.out
                        .event(CaptureEvent::Paused(PauseReason::SystemPaused));
                }
                false
            }
            // Idle frames carry no buffer: `Exact` repeats come from the
            // pacer, `OnDamage` waits for the next change.
            FrameStatus::Idle
            | FrameStatus::Blank
            | FrameStatus::Started
            | FrameStatus::Stopped
            | FrameStatus::Unknown(_) => false,
        }
    }

    fn deliver(&self, sample: &Sample) {
        if !self.status(sample.attachments().status()) || !sample.is_valid() {
            return;
        }
        if !self.out.wants_frames() {
            // Paused or ending: the surface goes back to SCK at once.
            self.skip(0);
            return;
        }
        let Some(pixels) = sample.pixel_buffer() else {
            return;
        };
        let pts = sample
            .pts_ns()
            .or_else(super::ffi::host_time_ns)
            .unwrap_or_default();
        let mut state = lock(&self.state);
        let size = pixels.size();
        let resized = state.size.is_some_and(|known| known != size);
        if resized {
            self.out.event(CaptureEvent::SourceResized(size));
        }
        state.size = Some(size);
        let pool = &self.pool;
        let depth = usize::from(self.depth);
        if resized {
            // Surfaces of another size: SCK allocated a new pool.
            state.slots.restart(pool.reallocate(depth));
        }
        let (slot, change) = state
            .slots
            .slot(pixels.surface_id(), || pool.reallocate(depth));
        if let SlotChange::Grew(seen) = change
            && seen > depth
        {
            // SCK cycles through more surfaces than the queue depth: the
            // pool is at least that deep, holds stay counted.
            pool.resize(seen);
        }
        let mut meta = meta_for(&state.shape, sample, &pixels, pts);
        meta.skipped_before = std::mem::take(&mut state.skipped);
        let shape = state.shape.clone();
        drop(state);
        let buffer = match self.pool.hold(slot) {
            Ok(lease) => {
                let identity = lease.identity();
                meta.buffer = identity;
                match backing(&shape, pixels) {
                    Ok(buffer) => buffer
                        .with_identity(identity.into())
                        .with_release(move || drop(lease)),
                    Err(err) => {
                        tracing::warn!(%err, "screen-capture: a frame could not be mapped");
                        self.skip(meta.skipped_before);
                        return;
                    }
                }
            }
            Err(HoldError::Exhausted(exhausted)) => {
                self.out.exhausted(exhausted, self.depth, self.exhaustion);
                match self.exhaustion {
                    Exhaustion::Stall | Exhaustion::DropNewest => {
                        // The surface goes back to SCK at once: the next
                        // delivered frame is fresh and says what it missed.
                        self.skip(meta.skipped_before);
                        return;
                    }
                    Exhaustion::CopyToCpu => match pixels.read() {
                        Ok(bytes) => {
                            meta.memory = BufferMemory::Cpu;
                            meta.copied = Some(CopyStage::ExhaustionReadback);
                            NativeBuffer::from_bytes(bytes)
                        }
                        Err(err) => {
                            tracing::warn!(%err, "screen-capture: exhaustion readback failed");
                            self.skip(meta.skipped_before);
                            return;
                        }
                    },
                }
            }
            Err(err @ HoldError::SlotOutOfRange { .. }) => {
                tracing::warn!(%err, "screen-capture: a surface outside the pool");
                self.skip(meta.skipped_before);
                return;
            }
        };
        // A closed sink ends the session through the host, which drops
        // the stream.
        let _delivery: Delivery = self.out.deliver(meta, buffer);
    }

    /// A frame released unseen: counted on the next delivered one.
    fn skip(&self, already: u32) {
        let mut state = lock(&self.state);
        state.skipped = state.skipped.saturating_add(already).saturating_add(1);
    }
}

impl Sink for LiveSink {
    fn sample(&self, kind: Kind, sample: Sample) {
        if kind == Kind::Screen {
            self.deliver(&sample);
        }
    }

    fn stopped(&self, error: CaptureError) {
        self.out.fail(error);
    }
}

/// The colour frames of `format` carry under `settings`.
pub(super) const fn colour_of(format: PixelFormat, hdr: bool) -> Colour {
    let range = match sck_map::FourCc::for_format(format) {
        Some(code) => match code.format() {
            Some((_, range)) => range,
            None => crate::types::ColourRange::Full,
        },
        None => crate::types::ColourRange::Full,
    };
    sck_map::colour(format, range, hdr)
}
