//! Per-frame metadata, the negotiated stream and session events.

use istmo_core::NativeHandleId;
use istmo_macros::message;

use super::caps::{
    BufferMemory, Colour, CopyStage, DamageSupport, GpuIdentity, OrientationSupport, PixelFormat,
    RegionCrop, SyncKind,
};
use super::config::{AudioSource, CursorMode, Exhaustion, FrameRate, SampleFormat};
use super::consent::{ConsentModel, IndicatorState, RestoreToken};
use super::error::CaptureError;
use super::source::{BackendId, Orientation, Point, Rect, Size, Source, SourceId};

/// The clock `pts_ns` is on.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PtsClock {
    /// `CLOCK_MONOTONIC` (PipeWire, Wayland, Android).
    Monotonic,
    /// Windows `QueryPerformanceCounter`.
    Qpc,
    /// `mach_continuous_time`.
    MachContinuous,
    /// `mach_absolute_time` (does not advance in sleep).
    HostTime,
    /// Web `performance.now`.
    Performance,
}

/// One plane of a frame.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlaneLayout {
    pub offset: u32,
    pub stride: u32,
    /// Bytes from `offset` to the end of the plane's last row.
    pub size: u64,
}

/// Identifies the pooled object behind a frame: same `(generation,
/// slot)` = same DMA-BUF / texture / `IOSurface`, so consumers cache
/// imports by it. `pool_generation` changes on every reallocation.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct BufferIdentity {
    pub pool_generation: u64,
    pub slot: u32,
}

impl From<istmo_core::BufferIdentity> for BufferIdentity {
    fn from(id: istmo_core::BufferIdentity) -> Self {
        Self {
            pool_generation: id.pool_generation,
            slot: id.slot,
        }
    }
}

impl From<BufferIdentity> for istmo_core::BufferIdentity {
    fn from(id: BufferIdentity) -> Self {
        Self::new(id.pool_generation, id.slot)
    }
}

/// What changed since the previous frame.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum Damage {
    #[default]
    Full,
    /// In buffer pixels.
    Rects(Vec<Rect>),
    Unchanged,
}

impl Damage {
    /// Rectangles a frame carries at most; more become [`Damage::Full`].
    pub const MAX_RECTS: usize = 32;

    /// Producer-reported rectangles clipped to a `size` buffer. Nothing
    /// left after clipping (or nothing reported), or more than
    /// [`MAX_RECTS`](Self::MAX_RECTS), is [`Damage::Full`]: a consumer can
    /// index the buffer by every rectangle it receives.
    #[must_use]
    pub fn clipped(rects: impl IntoIterator<Item = Rect>, size: Size) -> Self {
        let bounds = Rect::from_size(size);
        let mut kept = Vec::new();
        for rect in rects {
            let Some(inside) = rect.intersect(bounds) else {
                continue;
            };
            if kept.len() == Self::MAX_RECTS {
                return Self::Full;
            }
            kept.push(inside);
        }
        if kept.is_empty() {
            Self::Full
        } else {
            Self::Rects(kept)
        }
    }

    /// `self` followed by `next`: what changed across both.
    #[must_use]
    pub fn merge(self, next: Self) -> Self {
        match (self, next) {
            (Self::Full, _) | (_, Self::Full) => Self::Full,
            (Self::Unchanged, other) | (other, Self::Unchanged) => other,
            (Self::Rects(mut a), Self::Rects(b)) => {
                if a.len() + b.len() > Self::MAX_RECTS {
                    return Self::Full;
                }
                a.extend(b);
                Self::Rects(a)
            }
        }
    }

    /// Whether anything changed.
    #[must_use]
    pub const fn changed(&self) -> bool {
        !matches!(self, Self::Unchanged)
    }
}

/// Cursor state for `CursorMode::Metadata`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorState {
    /// Hotspot position in buffer pixels.
    pub position: Point,
    pub hotspot: Point,
    pub visible: bool,
}

/// One part of a composite frame.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompositePart {
    pub source: SourceId,
    /// Where the part sits in the composite buffer.
    pub rect: Rect,
    pub pts_ns: u64,
    pub scale_permille: u32,
}

/// How a composite frame was put together.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CompositeInfo {
    pub parts: Vec<CompositePart>,
    /// Largest difference between the parts' capture times.
    pub skew_ns: u64,
}

/// Static HDR metadata (SMPTE ST 2086 / CTA-861.3).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct HdrMetadata {
    pub max_luminance_nits: u32,
    /// In 1/10000 nits.
    pub min_luminance: u32,
    pub max_cll: u32,
    pub max_fall: u32,
}

/// Metadata of every video frame.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrameMeta {
    pub seq: u64,
    /// Capture time on `pts_clock`.
    pub pts_ns: u64,
    pub pts_clock: PtsClock,
    /// UNIX epoch µs, computed per frame from `pts_ns`.
    pub wall_us: u64,
    /// Buffer size.
    pub size: Size,
    /// Valid area (crop, letterbox), in buffer pixels.
    pub content_rect: Rect,
    pub format: PixelFormat,
    pub memory: BufferMemory,
    pub planes: Vec<PlaneLayout>,
    pub modifier: Option<u64>,
    pub buffer: BufferIdentity,
    pub tier: u8,
    /// This frame went through an occasional copy.
    pub copied: Option<CopyStage>,
    pub producer: Option<GpuIdentity>,
    pub colour: Colour,
    pub hdr: Option<HdrMetadata>,
    /// Still to apply: mirror, then quarter turns.
    pub orientation: Orientation,
    pub damage: Damage,
    pub cursor: Option<CursorState>,
    pub sync: SyncKind,
    pub scale_permille: u32,
    pub source_id: SourceId,
    /// A repeat of the previous frame (same buffer).
    pub repeat: bool,
    /// Frames the producer side skipped since the previous one.
    pub skipped_before: u32,
    /// The OS reports masked content in this frame.
    pub protected_content: bool,
    pub composite: Option<CompositeInfo>,
}

/// Metadata of every audio chunk (interleaved PCM in a CPU buffer).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AudioMeta {
    pub seq: u64,
    pub pts_ns: u64,
    pub wall_us: u64,
    pub rate: u32,
    pub channels: u8,
    pub format: SampleFormat,
    /// Frames (samples per channel) in this chunk.
    pub frames: u32,
    /// `wall_us` was re-anchored (a gap or drift).
    pub discontinuity: bool,
}

/// Audio as negotiated.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioNegotiated {
    pub source: AudioSource,
    pub rate: u32,
    pub channels: u8,
    pub format: SampleFormat,
}

/// The Windows capture border.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BorderState {
    Shown,
    Hidden,
    NotApplicable,
}

/// What a session (or screenshot) actually got.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NegotiatedStream {
    pub backend: BackendId,
    /// What was captured (the picker choice, when there was one).
    pub source: Source,
    pub size: Size,
    pub content_rect: Rect,
    pub format: PixelFormat,
    pub memory: BufferMemory,
    pub modifier: Option<u64>,
    pub tier: u8,
    /// Copies made on every frame.
    pub copies: Vec<CopyStage>,
    /// As enforced.
    pub fps: FrameRate,
    /// What the producer delivers.
    pub source_rate_mhz: Option<u32>,
    /// `None` until known (portal: first frame).
    pub producer: Option<GpuIdentity>,
    pub sync: SyncKind,
    pub pool_depth: u8,
    pub exhaustion: Exhaustion,
    pub orientation: OrientationSupport,
    pub crop: RegionCrop,
    pub damage: DamageSupport,
    pub cursor: CursorMode,
    pub colour: Colour,
    pub hdr: bool,
    pub composite: Option<CompositeInfo>,
    pub consent: ConsentModel,
    pub indicator: IndicatorState,
    pub border: BorderState,
    /// Same-process callers only; a remote caller gets `None`.
    pub restore_token: Option<RestoreToken>,
    pub restored: bool,
    pub audio: Option<AudioNegotiated>,
}

/// The answer to `screenshot`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScreenshotInfo {
    /// Frames already queued in the sink.
    pub frames: u32,
    pub backend: BackendId,
    pub negotiated: NegotiatedStream,
}

/// Phantom tag of a capture session handle.
#[derive(Debug, Clone, Copy)]
pub struct CaptureSessionTag(std::convert::Infallible);

/// The answer to `open*`. `open_owned` wraps `session` in a handle that
/// closes the session on drop.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CaptureSession {
    #[handle(CaptureSessionTag)]
    pub session: NativeHandleId,
    pub negotiated: NegotiatedStream,
}

/// Why a session moved to a lower tier.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DowngradeReason {
    /// The compositor refused a DMA-BUF (wlr `failed`).
    ImportRejected,
    AllocationFailed,
    ModifierUnreadable,
    ProducerChanged,
    PoolChanged,
}

/// A tier change, announced in band.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Downgrade {
    pub from_tier: u8,
    pub to_tier: u8,
    pub reason: DowngradeReason,
}

/// Why frames stopped for a while.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PauseReason {
    Backgrounded,
    SecureDesktop,
    SystemPaused,
    Requested,
}

/// One exhaustion episode.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExhaustionReport {
    /// Episodes so far in this session, this one included.
    pub episode: u32,
    pub pool_depth: u8,
    pub policy: Exhaustion,
}

/// Why a session ended.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StopReason {
    Closed,
    ConsentRevoked,
    SourceLost,
    Failed,
    Shutdown,
}

/// Session events.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CaptureEvent {
    Started(NegotiatedStream),
    Renegotiated(NegotiatedStream),
    Downgraded(Downgrade),
    SourceResized(Size),
    SourceHidden,
    SourceVisible,
    Paused(PauseReason),
    Resumed,
    Exhausted(ExhaustionReport),
    ConsentRevoked,
    SourceLost,
    Failed(CaptureError),
    Stopped(StopReason),
}

/// Phantom tag of an audio-only session handle.
#[derive(Debug, Clone, Copy)]
pub struct AudioSessionTag(std::convert::Infallible);

/// The answer to `open_audio`. `open_audio_owned` wraps `session` in a
/// handle that closes the session on drop.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioCaptureSession {
    #[handle(AudioSessionTag)]
    pub session: NativeHandleId,
    pub negotiated: AudioNegotiated,
}

/// Events of an audio-only session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AudioEvent {
    Started(AudioNegotiated),
    Paused(PauseReason),
    Resumed,
    Muted,
    Unmuted,
    Failed(CaptureError),
    Stopped(StopReason),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn producer_damage_is_clipped_to_the_buffer() {
        let size = Size::new(100, 50);
        let damage = Damage::clipped(
            [
                Rect::new(90, 40, 20, 20),
                Rect::new(-10, -10, 5, 5),
                Rect::new(i32::MAX, i32::MAX, u32::MAX, u32::MAX),
            ],
            size,
        );
        assert_eq!(damage, Damage::Rects(vec![Rect::new(90, 40, 10, 10)]));
        assert_eq!(
            Damage::clipped([Rect::new(-5, 0, 2, 2)], size),
            Damage::Full
        );
        assert_eq!(Damage::clipped([], size), Damage::Full);
    }

    #[test]
    fn too_many_rects_become_full() {
        let size = Size::new(1000, 1000);
        let many = (0..=33).map(|i| Rect::new(i, i, 1, 1));
        assert_eq!(Damage::clipped(many, size), Damage::Full);
        let half: Vec<Rect> = (0..20).map(|i| Rect::new(i, 0, 1, 1)).collect();
        let merged = Damage::Rects(half.clone()).merge(Damage::Rects(half));
        assert_eq!(merged, Damage::Full, "merging is capped too");
    }
}
