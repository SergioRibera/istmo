//! Frame metadata and negotiated streams as every Windows backend
//! builds them, and the pacing they share.

use std::time::Duration;

use crate::types::{
    BackendId, BorderState, BufferIdentity, BufferMemory, Colour, ColourRange, ColourSpace,
    ConsentModel, CopyStage, CursorMode, CursorState, Damage, DamageSupport, Exhaustion, FrameMeta,
    FrameRate, GpuIdentity, IndicatorState, NegotiatedStream, Orientation, OrientationSupport,
    PixelFormat, PlaneLayout, PtsClock, Rect, RegionCrop, Size, Source, SyncKind, TransferFunction,
};

/// scRGB: linear, BT.709 primaries, values beyond [0, 1] (WGC / DXGI
/// FP16).
pub(crate) const SCRGB: Colour = Colour {
    space: ColourSpace::ScRgb,
    transfer: TransferFunction::Linear,
    range: ColourRange::Full,
};

/// The colour a frame of `format` carries.
pub(crate) const fn colour_of(format: PixelFormat) -> Colour {
    match format {
        PixelFormat::Rgba16f => SCRGB,
        _ => Colour::SRGB,
    }
}

/// The period between captures at `fps` on a monitor refreshing at
/// `refresh_mhz` (60 Hz when unknown).
pub(crate) fn period(fps: FrameRate, refresh_mhz: Option<u32>) -> Duration {
    match fps {
        FrameRate::Native => {
            Duration::from_secs(1000) / refresh_mhz.filter(|r| *r > 0).unwrap_or(60_000)
        }
        FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => {
            Duration::from_secs(1) / n.max(1)
        }
    }
}

/// What one frame is, before the host fills `seq` and `wall_us`.
#[derive(Debug, Clone)]
pub(crate) struct FrameFacts {
    pub(crate) size: Size,
    pub(crate) format: PixelFormat,
    pub(crate) memory: BufferMemory,
    /// Row stride of CPU frames, 0 for textures.
    pub(crate) stride: u32,
    /// `QueryPerformanceCounter` time of the capture, ns.
    pub(crate) pts_ns: u64,
    pub(crate) damage: Damage,
    pub(crate) cursor: Option<CursorState>,
    pub(crate) orientation: Orientation,
    pub(crate) producer: Option<GpuIdentity>,
    pub(crate) protected: bool,
}

/// The `FrameMeta` of a captured frame of `source`.
pub(crate) fn frame_meta(source: &Source, facts: FrameFacts) -> FrameMeta {
    let planes = if facts.memory == BufferMemory::Cpu {
        vec![PlaneLayout {
            offset: 0,
            stride: facts.stride,
            size: u64::from(facts.stride) * u64::from(facts.size.height),
        }]
    } else {
        Vec::new()
    };
    FrameMeta {
        seq: 0,
        pts_ns: facts.pts_ns,
        pts_clock: PtsClock::Qpc,
        wall_us: 0,
        size: facts.size,
        content_rect: Rect::from_size(facts.size),
        format: facts.format,
        memory: facts.memory,
        planes,
        modifier: None,
        buffer: BufferIdentity::default(),
        tier: 0,
        copied: None,
        producer: facts.producer,
        colour: colour_of(facts.format),
        hdr: None,
        orientation: facts.orientation,
        damage: facts.damage,
        cursor: facts.cursor,
        sync: SyncKind::Ready,
        scale_permille: 1000,
        source_id: source.id.clone(),
        repeat: false,
        skipped_before: 0,
        protected_content: facts.protected,
        composite: None,
    }
}

/// What a session negotiated, as every Windows backend reports it.
#[derive(Debug, Clone)]
pub(crate) struct StreamFacts {
    pub(crate) backend: BackendId,
    pub(crate) size: Size,
    pub(crate) format: PixelFormat,
    pub(crate) memory: BufferMemory,
    pub(crate) tier: u8,
    pub(crate) copies: Vec<CopyStage>,
    pub(crate) pool_depth: u8,
    pub(crate) exhaustion: Exhaustion,
    pub(crate) orientation: OrientationSupport,
    pub(crate) damage: DamageSupport,
    pub(crate) cursor: CursorMode,
    pub(crate) producer: Option<GpuIdentity>,
    pub(crate) border: BorderState,
}

/// The negotiated stream of a session on `source`.
pub(crate) fn negotiated(source: &Source, facts: StreamFacts) -> NegotiatedStream {
    NegotiatedStream {
        backend: facts.backend,
        source: source.clone(),
        size: facts.size,
        content_rect: Rect::from_size(facts.size),
        format: facts.format,
        memory: facts.memory,
        modifier: None,
        tier: facts.tier,
        copies: facts.copies,
        fps: FrameRate::Native,
        source_rate_mhz: source.refresh_mhz,
        producer: facts.producer,
        sync: SyncKind::Ready,
        pool_depth: facts.pool_depth,
        exhaustion: facts.exhaustion,
        orientation: facts.orientation,
        crop: RegionCrop::InMeta,
        damage: facts.damage,
        cursor: facts.cursor,
        colour: colour_of(facts.format),
        hdr: facts.format == PixelFormat::Rgba16f,
        composite: None,
        consent: ConsentModel::NoneNeeded,
        // The plugin reports what it knows: WGC's yellow border is the
        // OS's indicator (`border`); DXGI and GDI show nothing.
        indicator: match facts.border {
            BorderState::Shown => IndicatorState::Shown,
            BorderState::Hidden | BorderState::NotApplicable => IndicatorState::NotApplicable,
        },
        border: facts.border,
        restore_token: None,
        restored: false,
        audio: None,
    }
}

/// Damage rectangles a producer reported, as frame damage: clipped to
/// the buffer, more than `Damage::MAX_RECTS` (or none) become `Full`.
pub(crate) fn damage_of(rects: impl IntoIterator<Item = Rect>, size: Size) -> Damage {
    Damage::clipped(rects, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_pacing_follows_the_refresh() {
        assert_eq!(
            period(FrameRate::Native, Some(120_000)),
            Duration::from_secs(1) / 120
        );
        assert_eq!(period(FrameRate::Native, None), Duration::from_secs(1) / 60);
        assert_eq!(period(FrameRate::Max(0), None), Duration::from_secs(1));
    }

    #[test]
    fn fp16_frames_are_scrgb() {
        assert_eq!(colour_of(PixelFormat::Rgba16f), SCRGB);
        assert_eq!(colour_of(PixelFormat::Bgra8), Colour::SRGB);
    }
}
