//! ScreenCaptureKit facts as plain data: pixel formats, the capability
//! table per macOS version, frame attachments (status, content rect,
//! scale, dirty rects) mapped onto `FrameMeta`, stop errors, buffer
//! slots, sizes and audio PCM. No Apple framework here, so the rules
//! are unit-tested on any host; `macos/sck` feeds them what the
//! framework reports.

use crate::types::{
    AccessScope, AccessState, AccessStatus, AudioCaps, AudioSourceKind, BackendCaps, BackendId,
    BufferMemory, CapsProvenance, CaptureError, Colour, ColourCaps, ColourRange, ColourSpace,
    CompositeCaps, CompositeSupport, ConsentModel, CopyStage, CursorMode, Damage, DamageSupport,
    Exhaustion, FormatCaps, FrameRate, IndicatorState, OrientationSupport, OsVersion, OutputSize,
    Persistence, PixelFormat, PoolCaps, ProtectedContent, Provenance, Rect, RegionCrop,
    SampleFormat, Scaling, Size, SourceKind, SourceListing, SyncKind, TransferFunction,
    WindowCaptureCaps,
};

/// The backend these facts describe.
pub(crate) const ID: BackendId = BackendId::MacosSck;

/// `SCStream`, `SCShareableContent`: the floor of the backend.
pub(crate) const SCK: OsVersion = version(12, 3);
/// `capturesAudio`, `sampleRate`, `channelCount`,
/// `excludesCurrentProcessAudio`.
pub(crate) const AUDIO: OsVersion = version(13, 0);
/// `SCWindow.isActive`.
pub(crate) const WINDOW_ACTIVE: OsVersion = version(13, 1);
/// `SCContentSharingPicker`, `SCScreenshotManager`, the filter's
/// `style` / `contentRect` / `pointPixelScale`.
pub(crate) const PICKER: OsVersion = version(14, 0);
/// `includeChildWindows`.
pub(crate) const CHILD_WINDOWS: OsVersion = version(14, 2);
/// `captureDynamicRange` (HDR).
pub(crate) const HDR: OsVersion = version(15, 0);

const fn version(major: u32, minor: u32) -> OsVersion {
    OsVersion {
        major,
        minor,
        patch: 0,
    }
}

/// Whether `os` is at least `floor`.
#[must_use]
pub(crate) const fn at_least(os: OsVersion, floor: OsVersion) -> bool {
    os.major > floor.major
        || (os.major == floor.major
            && (os.minor > floor.minor || (os.minor == floor.minor && os.patch >= floor.patch)))
}

/// `SCStreamConfiguration.queueDepth`: default and minimum 3, "don't
/// exceed" 8. The plugin's default leaves the consumer two frames.
pub(crate) const POOL: PoolCaps = PoolCaps {
    min_depth: 3,
    max_depth: 8,
    default_depth: 4,
    exhaustion: Vec::new(),
};

/// A Core Video pixel format code (`OSType`, four ASCII bytes, big end
/// first: `'BGRA'` is `0x4247_5241`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct FourCc(pub(crate) u32);

impl FourCc {
    /// `kCVPixelFormatType_32BGRA`.
    pub(crate) const BGRA: Self = Self::of(*b"BGRA");
    /// `kCVPixelFormatType_ARGB2101010LEPacked`.
    pub(crate) const L10R: Self = Self::of(*b"l10r");
    /// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
    pub(crate) const YUV420_VIDEO: Self = Self::of(*b"420v");
    /// `kCVPixelFormatType_420YpCbCr8BiPlanarFullRange`.
    pub(crate) const YUV420_FULL: Self = Self::of(*b"420f");
    /// `kCVPixelFormatType_444YpCbCr10BiPlanarFullRange`: SCK offers it,
    /// `PixelFormat` has no 10-bit 4:4:4 biplanar entry, so it is never
    /// asked for.
    #[allow(
        dead_code,
        reason = "names a format SCK offers that `PixelFormat` cannot; only the tests read it"
    )]
    pub(crate) const XF44: Self = Self::of(*b"xf44");
    /// `kCVPixelFormatType_64RGBAHalf`.
    pub(crate) const RGBA_HALF: Self = Self::of(*b"RGhA");

    const fn of(code: [u8; 4]) -> Self {
        Self(u32::from_be_bytes(code))
    }

    /// The plugin's format and the range this code encodes.
    #[must_use]
    pub(crate) const fn format(self) -> Option<(PixelFormat, ColourRange)> {
        match self {
            Self::BGRA => Some((PixelFormat::Bgra8, ColourRange::Full)),
            // ARGB2101010 in a little-endian 32-bit word: DRM AR30.
            Self::L10R => Some((PixelFormat::Ar30, ColourRange::Full)),
            Self::YUV420_VIDEO => Some((PixelFormat::Nv12, ColourRange::Limited)),
            Self::YUV420_FULL => Some((PixelFormat::Nv12, ColourRange::Full)),
            Self::RGBA_HALF => Some((PixelFormat::Rgba16f, ColourRange::Full)),
            _ => None,
        }
    }

    /// The code SCK is asked for to deliver `format` (NV12 in video
    /// range, what hardware encoders take).
    #[must_use]
    pub(crate) const fn for_format(format: PixelFormat) -> Option<Self> {
        match format {
            PixelFormat::Bgra8 => Some(Self::BGRA),
            PixelFormat::Ar30 => Some(Self::L10R),
            PixelFormat::Nv12 => Some(Self::YUV420_VIDEO),
            PixelFormat::Rgba16f => Some(Self::RGBA_HALF),
            PixelFormat::Bgrx8
            | PixelFormat::Rgba8
            | PixelFormat::Rgbx8
            | PixelFormat::Xr30
            | PixelFormat::Xb30
            | PixelFormat::P010 => None,
        }
    }
}

/// The formats SCK delivers on `os`, best first.
#[must_use]
pub(crate) fn formats(os: OsVersion) -> Vec<PixelFormat> {
    let mut formats = vec![PixelFormat::Bgra8, PixelFormat::Ar30, PixelFormat::Nv12];
    if at_least(os, HDR) {
        formats.push(PixelFormat::Rgba16f);
    }
    formats
}

/// Every memory a frame can be handed over in, none of them a copy:
/// the retained `IOSurface`, the `CVPixelBuffer` around it, or the
/// surface's own memory locked read-only.
pub(crate) const MEMORIES: [BufferMemory; 3] = [
    BufferMemory::IoSurface,
    BufferMemory::CvPixelBuffer,
    BufferMemory::Cpu,
];

/// The colour a frame in `format` and `range` carries.
#[must_use]
pub(crate) const fn colour(format: PixelFormat, range: ColourRange, hdr: bool) -> Colour {
    match format {
        PixelFormat::Rgba16f => Colour {
            space: ColourSpace::ScRgb,
            transfer: TransferFunction::Linear,
            range: ColourRange::Full,
        },
        PixelFormat::Nv12 => Colour {
            space: ColourSpace::Bt709,
            transfer: TransferFunction::Bt709,
            range,
        },
        _ if hdr => Colour {
            space: ColourSpace::DisplayP3,
            transfer: TransferFunction::Pq,
            range: ColourRange::Full,
        },
        _ => Colour::SRGB,
    }
}

/// What `caps` says about audio on `os`.
#[must_use]
pub(crate) fn audio_caps(os: OsVersion) -> AudioCaps {
    if !at_least(os, AUDIO) {
        return AudioCaps {
            sources: Vec::new(),
            formats: Vec::new(),
            may_be_silent: false,
        };
    }
    AudioCaps {
        sources: vec![
            AudioSourceKind::System,
            AudioSourceKind::Application,
            AudioSourceKind::ExcludeSelf,
        ],
        formats: vec![SampleFormat::F32, SampleFormat::S16],
        may_be_silent: false,
    }
}

/// The capability report on `os`; `granted` is the Screen Recording
/// grant (`CGPreflightScreenCaptureAccess`), which decides whether
/// sources are enumerable yet.
#[must_use]
pub(crate) fn caps(os: OsVersion, granted: bool) -> BackendCaps {
    let mut caps = BackendCaps::new(ID);
    caps.source_kinds = vec![
        SourceKind::Monitor,
        SourceKind::Window,
        SourceKind::Application,
    ];
    caps.listing = if granted {
        SourceListing::Enumerable
    } else {
        SourceListing::AfterGrant
    };
    caps.consent = ConsentModel::SystemSetting;
    caps.video = formats(os)
        .into_iter()
        .flat_map(|format| {
            MEMORIES.into_iter().map(move |memory| FormatCaps {
                format,
                memory,
                modifiers: Vec::new(),
                copies: Vec::new(),
            })
        })
        .collect();
    caps.scaling = Scaling::Native;
    // Regions travel in `content_rect`, as on every desktop backend; SCK's
    // `sourceRect` is not used yet.
    caps.region = RegionCrop::InMeta;
    caps.composite = CompositeCaps {
        screenshot: CompositeSupport::CpuOnly,
        streaming: CompositeSupport::Unsupported,
    };
    caps.cursor = vec![CursorMode::Hidden, CursorMode::Embedded];
    caps.damage = DamageSupport::Rects;
    // The window server composes rotated displays upright.
    caps.orientation = OrientationSupport::AlreadyApplied;
    let hdr = at_least(os, HDR);
    caps.colour = ColourCaps {
        colours: if hdr {
            vec![
                Colour::SRGB,
                colour(PixelFormat::Ar30, ColourRange::Full, true),
                colour(PixelFormat::Rgba16f, ColourRange::Full, true),
            ]
        } else {
            vec![Colour::SRGB]
        },
        hdr,
    };
    caps.audio = audio_caps(os);
    caps.sync = vec![SyncKind::Ready];
    caps.persistence = Persistence::SystemSetting;
    caps.pool = PoolCaps {
        exhaustion: vec![
            Exhaustion::Stall,
            Exhaustion::DropNewest,
            Exhaustion::CopyToCpu,
        ],
        ..POOL
    };
    caps.copies = vec![CopyStage::ExhaustionReadback];
    // `NSWindow.sharingType = .none` windows are left out of the frame
    // without a trace.
    caps.protected_content = ProtectedContent::MaskedUndetectable;
    caps.window = WindowCaptureCaps {
        occluded: true,
        minimised: false,
        child_popups: at_least(os, CHILD_WINDOWS),
        border: false,
    };
    caps.provenance = CapsProvenance {
        source_kinds: Provenance::Table,
        listing: Provenance::Probe,
        video: Provenance::Table,
        cursor: Provenance::Table,
        damage: Provenance::Table,
        colour: Provenance::Table,
        audio: Provenance::Table,
        producer: Provenance::UnknownUntilNegotiated,
        sync: Provenance::Table,
        persistence: Provenance::Table,
    };
    caps
}

/// What a source id of this backend names: `display:<CGDirectDisplayID>`,
/// `window:<CGWindowID>`, `app:<bundle id>` or `pid:<process id>` (an
/// application without a bundle id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SourceName {
    Display(u32),
    Window(u32),
    Application {
        bundle_id: Option<String>,
        pid: Option<i32>,
    },
}

impl SourceName {
    /// Parse a source id's value; `None` when it is not one of ours.
    #[must_use]
    pub(crate) fn parse(value: &str) -> Option<Self> {
        let (kind, rest) = value.split_once(':')?;
        match kind {
            "display" => rest.parse().ok().map(Self::Display),
            "window" => rest.parse().ok().map(Self::Window),
            "app" if !rest.is_empty() => Some(Self::Application {
                bundle_id: Some(rest.to_owned()),
                pid: None,
            }),
            "pid" => rest.parse().ok().map(|pid| Self::Application {
                bundle_id: None,
                pid: Some(pid),
            }),
            _ => None,
        }
    }

    /// The id value naming this source.
    #[must_use]
    pub(crate) fn value(&self) -> String {
        match self {
            Self::Display(id) => format!("display:{id}"),
            Self::Window(id) => format!("window:{id}"),
            Self::Application {
                bundle_id: Some(bundle),
                ..
            } => format!("app:{bundle}"),
            Self::Application {
                bundle_id: None,
                pid,
            } => format!("pid:{}", pid.unwrap_or_default()),
        }
    }
}
/// `SCFrameStatus`, from a frame's `SCStreamFrameInfoStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FrameStatus {
    /// A new frame.
    Complete,
    /// Nothing changed; no pixel buffer.
    Idle,
    /// A blank frame; no content.
    Blank,
    /// The stream is suspended (the display slept, the content went
    /// away for a while).
    Suspended,
    Started,
    Stopped,
    /// A value this table does not know.
    Unknown(i64),
}

impl From<i64> for FrameStatus {
    fn from(raw: i64) -> Self {
        match raw {
            0 => Self::Complete,
            1 => Self::Idle,
            2 => Self::Blank,
            3 => Self::Suspended,
            4 => Self::Started,
            5 => Self::Stopped,
            other => Self::Unknown(other),
        }
    }
}

/// A Core Graphics rectangle (points or pixels, as the key says).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct RectF {
    pub(crate) x: f64,
    pub(crate) y: f64,
    pub(crate) width: f64,
    pub(crate) height: f64,
}

impl RectF {
    /// The smallest integer rectangle covering `self` scaled by `scale`
    /// (`None` when empty or not finite).
    #[must_use]
    pub(crate) fn covering(self, scale: f64) -> Option<Rect> {
        let values = [self.x, self.y, self.width, self.height, scale];
        if values.iter().any(|v| !v.is_finite()) || self.width <= 0.0 || self.height <= 0.0 {
            return None;
        }
        let left = (self.x * scale).floor();
        let top = (self.y * scale).floor();
        let right = ((self.x + self.width) * scale).ceil();
        let bottom = ((self.y + self.height) * scale).ceil();
        let clamp = |v: f64| v.clamp(f64::from(i32::MIN), f64::from(i32::MAX));
        #[expect(
            clippy::cast_possible_truncation,
            reason = "clamped into i32 range and integral after floor / ceil"
        )]
        let (left, top, right, bottom) = (
            clamp(left) as i32,
            clamp(top) as i32,
            clamp(right) as i64,
            clamp(bottom) as i64,
        );
        let width = u32::try_from(right - i64::from(left)).ok()?;
        let height = u32::try_from(bottom - i64::from(top)).ok()?;
        (width > 0 && height > 0).then(|| Rect::new(left, top, width, height))
    }

    /// The rectangle rounded to whole units (logical geometry).
    #[must_use]
    pub(crate) fn rounded(self) -> Option<Rect> {
        let values = [self.x, self.y, self.width, self.height];
        if values.iter().any(|v| !v.is_finite()) || self.width < 0.0 || self.height < 0.0 {
            return None;
        }
        let clamp_i = |v: f64| v.round().clamp(f64::from(i32::MIN), f64::from(i32::MAX));
        let clamp_u = |v: f64| v.round().clamp(0.0, f64::from(u32::MAX));
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "rounded and clamped into the target range"
        )]
        Some(Rect::new(
            clamp_i(self.x) as i32,
            clamp_i(self.y) as i32,
            clamp_u(self.width) as u32,
            clamp_u(self.height) as u32,
        ))
    }
}

/// A scale factor as permille (`2.0` → 2000); 1000 when unusable.
#[must_use]
pub(crate) fn permille(scale: f64) -> u32 {
    if !scale.is_finite() || scale <= 0.0 {
        return 1000;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "positive, finite and clamped to u32"
    )]
    let permille = (scale * 1000.0).round().min(f64::from(u32::MAX)) as u32;
    permille.max(1)
}

/// The frame attachments that matter, as SCK reported them.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Attachments {
    /// `SCStreamFrameInfoStatus`.
    pub(crate) status: Option<i64>,
    /// `SCStreamFrameInfoContentRect`, points in the surface.
    pub(crate) content_rect: Option<RectF>,
    /// `SCStreamFrameInfoScaleFactor`: pixels per point.
    pub(crate) scale_factor: Option<f64>,
    /// `SCStreamFrameInfoDirtyRects`, pixels.
    pub(crate) dirty_rects: Option<Vec<RectF>>,
}

impl Attachments {
    /// The frame status; a frame without one is a complete frame
    /// (screenshots carry no status).
    #[must_use]
    pub(crate) fn status(&self) -> FrameStatus {
        self.status.map_or(FrameStatus::Complete, FrameStatus::from)
    }

    /// The valid area of a `size` buffer, in pixels.
    #[must_use]
    pub(crate) fn content_rect(&self, size: Size) -> Rect {
        let full = Rect::from_size(size);
        self.content_rect
            .and_then(|rect| rect.covering(self.scale_factor.unwrap_or(1.0)))
            .and_then(|rect| rect.intersect(full))
            .unwrap_or(full)
    }

    /// What changed in a `size` buffer.
    #[must_use]
    pub(crate) fn damage(&self, size: Size) -> Damage {
        match &self.dirty_rects {
            // No key: SCK said nothing, so all of it may have changed.
            None => Damage::Full,
            Some(rects) if rects.is_empty() => Damage::Unchanged,
            Some(rects) => Damage::clipped(rects.iter().filter_map(|r| r.covering(1.0)), size),
        }
    }

    /// The frame's scale, permille.
    #[must_use]
    pub(crate) fn scale_permille(&self) -> u32 {
        self.scale_factor.map_or(1000, permille)
    }
}

/// A `CMTime` (`value / timescale` seconds) in nanoseconds; `None` for
/// an invalid or negative time.
#[must_use]
pub(crate) fn cmtime_ns(value: i64, timescale: i32) -> Option<u64> {
    if timescale <= 0 || value < 0 {
        return None;
    }
    let ns = i128::from(value) * 1_000_000_000 / i128::from(timescale);
    u64::try_from(ns).ok()
}

/// `SCStreamConfiguration.minimumFrameInterval` for `fps`, as
/// `(value, timescale)`; `(0, 1)` (`kCMTimeZero`) is the display's
/// own rate.
#[must_use]
pub(crate) fn frame_interval(fps: FrameRate) -> (i64, i32) {
    match fps {
        FrameRate::Native => (0, 1),
        FrameRate::Max(n) | FrameRate::Exact(n) | FrameRate::OnDamage(n) => {
            (1, i32::try_from(n.max(1)).unwrap_or(i32::MAX))
        }
    }
}

/// The pixel size to ask SCK for: the source's own, or what the caller
/// asked (`Fit` keeps the aspect ratio inside the box).
#[must_use]
pub(crate) fn output_size(requested: OutputSize, native: Size) -> Size {
    match requested {
        OutputSize::Native => native,
        OutputSize::Scaled(size) => size,
        OutputSize::Fit(bound) => {
            if native.is_empty() || bound.is_empty() {
                return bound;
            }
            let (nw, nh) = (u64::from(native.width), u64::from(native.height));
            let (bw, bh) = (u64::from(bound.width), u64::from(bound.height));
            // Width-limited when native is wider than the box.
            let (w, h) = if nw * bh >= nh * bw {
                (bw, (nh * bw).div_ceil(nw))
            } else {
                ((nw * bh).div_ceil(nh), bh)
            };
            Size::new(
                u32::try_from(w).unwrap_or(u32::MAX).max(1),
                u32::try_from(h).unwrap_or(u32::MAX).max(1),
            )
        }
    }
}

/// `SCStreamErrorDomain` codes the plugin tells apart.
pub(crate) mod code {
    pub(crate) const USER_DECLINED: i64 = -3801;
    pub(crate) const NO_WINDOW_LIST: i64 = -3813;
    pub(crate) const NO_DISPLAY_LIST: i64 = -3814;
    pub(crate) const NO_CAPTURE_SOURCE: i64 = -3815;
    pub(crate) const REMOVING_STREAM: i64 = -3816;
    pub(crate) const USER_STOPPED: i64 = -3817;
    pub(crate) const SYSTEM_STOPPED_STREAM: i64 = -3821;
}

/// The error a stream stop (`stream:didStopWithError:`) or a failed
/// call with `SCStreamErrorDomain` `code` means.
#[must_use]
pub(crate) fn sck_error(operation: &str, code: i64) -> CaptureError {
    match code {
        code::USER_DECLINED => CaptureError::ConsentDenied,
        // The person stopped sharing (menu bar, picker).
        code::USER_STOPPED => CaptureError::ConsentRevoked,
        code::NO_CAPTURE_SOURCE
        | code::NO_DISPLAY_LIST
        | code::NO_WINDOW_LIST
        | code::REMOVING_STREAM
        | code::SYSTEM_STOPPED_STREAM => CaptureError::SourceLost,
        other => CaptureError::os(ID, operation, other),
    }
}

/// The Screen Recording grant as `access` reports it.
///
/// `granted` is `CGPreflightScreenCaptureAccess`. TCC tells no "denied"
/// from "never asked", and a grant given in System Settings applies only
/// to a relaunched process: once this process asked (`requested`) and is
/// still not granted, the answer is `RequiresRestart`.
#[must_use]
pub(crate) const fn screen_recording(granted: bool, requested: bool) -> AccessState {
    AccessState {
        scope: AccessScope::ScreenRecording,
        status: match (granted, requested) {
            (true, _) => AccessStatus::Granted,
            (false, true) => AccessStatus::RequiresRestart,
            (false, false) => AccessStatus::NotDetermined,
        },
        model: ConsentModel::SystemSetting,
        persistence: Persistence::SystemSetting,
        revocable_mid_session: true,
        // The menu bar's screen-recording indicator.
        indicator: IndicatorState::Shown,
    }
}

/// Buffer slots for the `IOSurface`s of one stream's pool: each surface
/// id gets a slot the first time it is seen; a surface beyond `depth`
/// means SCK reallocated its pool, so the slots start over in a new
/// generation.
#[derive(Debug)]
pub(crate) struct SlotMap {
    depth: usize,
    ids: Vec<u32>,
    generation: u64,
}

impl SlotMap {
    #[must_use]
    pub(crate) fn new(depth: usize, generation: u64) -> Self {
        Self {
            depth: depth.max(1),
            ids: Vec::new(),
            generation,
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) const fn generation(&self) -> u64 {
        self.generation
    }

    /// The slot of surface `id`, and whether the pool was reallocated
    /// first (`next_generation` is asked for the new generation).
    pub(crate) fn slot(&mut self, id: u32, next_generation: impl FnOnce() -> u64) -> (u32, bool) {
        if let Some(index) = self.ids.iter().position(|known| *known == id) {
            return (u32::try_from(index).unwrap_or(u32::MAX), false);
        }
        let reallocated = self.ids.len() >= self.depth;
        if reallocated {
            self.ids.clear();
            self.generation = next_generation();
        }
        self.ids.push(id);
        (
            u32::try_from(self.ids.len() - 1).unwrap_or(u32::MAX),
            reallocated,
        )
    }
}

/// Interleave planar `f32` channels (what SCK delivers) into `format`.
/// Channels of unequal length are cut to the shortest.
#[must_use]
pub(crate) fn interleave(planes: &[&[f32]], format: SampleFormat) -> (Vec<u8>, usize) {
    let frames = planes.iter().map(|p| p.len()).min().unwrap_or(0);
    let mut out = Vec::with_capacity(frames * planes.len() * format.bytes() as usize);
    for frame in 0..frames {
        for plane in planes {
            let sample = plane.get(frame).copied().unwrap_or(0.0);
            match format {
                SampleFormat::F32 => out.extend_from_slice(&sample.to_le_bytes()),
                SampleFormat::S16 => out.extend_from_slice(&to_s16(sample).to_le_bytes()),
            }
        }
    }
    (out, frames)
}

/// Convert interleaved `f32` samples into `format`.
#[must_use]
pub(crate) fn convert_interleaved(samples: &[f32], format: SampleFormat) -> Vec<u8> {
    let mut out = Vec::with_capacity(samples.len() * format.bytes() as usize);
    for sample in samples {
        match format {
            SampleFormat::F32 => out.extend_from_slice(&sample.to_le_bytes()),
            SampleFormat::S16 => out.extend_from_slice(&to_s16(*sample).to_le_bytes()),
        }
    }
    out
}

fn to_s16(sample: f32) -> i16 {
    let clamped = if sample.is_finite() {
        sample.clamp(-1.0, 1.0)
    } else {
        0.0
    };
    #[expect(
        clippy::cast_possible_truncation,
        reason = "clamped to [-1, 1], so the product fits i16"
    )]
    let value = (clamped * f32::from(i16::MAX)).round() as i16;
    value
}

/// Largest gap between where a chunk should start (the previous one's
/// end) and its timestamp before it counts as a discontinuity.
pub(crate) const AUDIO_GAP_NS: u64 = 100_000_000;

/// Follows audio chunk timestamps: a chunk that does not start where
/// the previous one ended (a gap, an overlap) is a discontinuity.
#[derive(Debug, Default)]
pub(crate) struct AudioTimeline {
    next_ns: Option<u64>,
}

impl AudioTimeline {
    /// Record a chunk of `frames` at `rate` starting at `pts_ns`; whether
    /// it breaks the timeline.
    pub(crate) fn chunk(&mut self, pts_ns: u64, frames: u64, rate: u32) -> bool {
        let gap = self
            .next_ns
            .is_some_and(|expected| expected.abs_diff(pts_ns) > AUDIO_GAP_NS);
        let duration = frames.saturating_mul(1_000_000_000) / u64::from(rate.max(1));
        self.next_ns = Some(pts_ns.saturating_add(duration));
        gap
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions_compare_by_major_minor_patch() {
        assert!(at_least(version(12, 3), SCK));
        assert!(!at_least(version(12, 2), SCK));
        assert!(at_least(version(15, 0), PICKER));
        assert!(!at_least(version(14, 1), CHILD_WINDOWS));
        let patch = OsVersion {
            major: 13,
            minor: 1,
            patch: 2,
        };
        assert!(at_least(patch, WINDOW_ACTIVE));
    }

    #[test]
    fn fourccs_read_as_apple_writes_them() {
        assert_eq!(FourCc::BGRA.0, 0x4247_5241);
        assert_eq!(FourCc::YUV420_VIDEO.0, 0x3432_3076);
        assert_eq!(
            FourCc::YUV420_FULL.format(),
            Some((PixelFormat::Nv12, ColourRange::Full))
        );
        assert_eq!(FourCc::XF44.format(), None);
        for format in formats(version(15, 0)) {
            let code = FourCc::for_format(format).expect("asked for");
            assert_eq!(code.format().map(|(f, _)| f), Some(format));
        }
        assert_eq!(FourCc::for_format(PixelFormat::P010), None);
    }

    #[test]
    fn caps_follow_the_os_and_the_grant() {
        let monterey = caps(version(12, 3), false);
        assert_eq!(monterey.listing, SourceListing::AfterGrant);
        assert_eq!(monterey.audio.sources, Vec::new(), "audio needs 13");
        assert!(!monterey.colour.hdr);
        assert!(!monterey.window.child_popups);
        assert!(
            !monterey
                .video
                .iter()
                .any(|f| f.format == PixelFormat::Rgba16f)
        );
        let sequoia = caps(version(15, 1), true);
        assert_eq!(sequoia.listing, SourceListing::Enumerable);
        assert!(sequoia.colour.hdr);
        assert!(sequoia.window.child_popups);
        assert!(
            sequoia
                .audio
                .sources
                .contains(&AudioSourceKind::ExcludeSelf)
        );
        // Every format in every memory, none of them a copy.
        assert_eq!(sequoia.video.len(), 4 * MEMORIES.len());
        assert!(sequoia.video.iter().all(|f| f.copies.is_empty()));
        assert_eq!(sequoia.pool.min_depth, 3);
        assert_eq!(sequoia.pool.max_depth, 8);
        assert_eq!(sequoia.provenance.listing, Provenance::Probe);
        assert_eq!(
            sequoia.provenance.producer,
            Provenance::UnknownUntilNegotiated
        );
    }

    #[test]
    fn statuses_map_and_a_missing_one_is_complete() {
        assert_eq!(FrameStatus::from(1), FrameStatus::Idle);
        assert_eq!(FrameStatus::from(9), FrameStatus::Unknown(9));
        assert_eq!(Attachments::default().status(), FrameStatus::Complete);
    }

    #[test]
    fn content_rect_is_points_times_scale_inside_the_buffer() {
        let size = Size::new(3024, 1964);
        let attachments = Attachments {
            content_rect: Some(RectF {
                x: 0.0,
                y: 0.0,
                width: 1512.0,
                height: 982.0,
            }),
            scale_factor: Some(2.0),
            ..Attachments::default()
        };
        assert_eq!(attachments.content_rect(size), Rect::from_size(size));
        assert_eq!(attachments.scale_permille(), 2000);
        // A letterboxed window: a smaller rect, offset.
        let window = Attachments {
            content_rect: Some(RectF {
                x: 10.5,
                y: 0.0,
                width: 100.0,
                height: 50.0,
            }),
            scale_factor: Some(2.0),
            ..Attachments::default()
        };
        assert_eq!(
            window.content_rect(Size::new(400, 100)),
            Rect::new(21, 0, 200, 100)
        );
        // Outside the buffer: the whole buffer.
        let off = Attachments {
            content_rect: Some(RectF {
                x: 5000.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            }),
            ..Attachments::default()
        };
        assert_eq!(off.content_rect(Size::new(4, 4)), Rect::new(0, 0, 4, 4));
    }

    #[test]
    fn dirty_rects_are_pixels_clipped_and_empty_means_unchanged() {
        let size = Size::new(100, 50);
        assert_eq!(Attachments::default().damage(size), Damage::Full);
        let none = Attachments {
            dirty_rects: Some(Vec::new()),
            ..Attachments::default()
        };
        assert_eq!(none.damage(size), Damage::Unchanged);
        let some = Attachments {
            dirty_rects: Some(vec![
                RectF {
                    x: 90.5,
                    y: 40.0,
                    width: 20.0,
                    height: 20.0,
                },
                RectF {
                    x: 0.0,
                    y: 0.0,
                    width: 0.0,
                    height: 3.0,
                },
            ]),
            ..Attachments::default()
        };
        assert_eq!(
            some.damage(size),
            Damage::Rects(vec![Rect::new(90, 40, 10, 10)])
        );
    }

    #[test]
    fn non_finite_rects_are_ignored() {
        let bad = RectF {
            x: f64::NAN,
            y: 0.0,
            width: 1.0,
            height: 1.0,
        };
        assert_eq!(bad.covering(1.0), None);
        assert_eq!(bad.rounded(), None);
        assert_eq!(permille(f64::INFINITY), 1000);
        let display = RectF {
            x: -1512.0,
            y: 0.4,
            width: 1512.0,
            height: 982.0,
        };
        assert_eq!(display.rounded(), Some(Rect::new(-1512, 0, 1512, 982)));
    }

    #[test]
    fn cmtimes_become_nanoseconds() {
        assert_eq!(cmtime_ns(3, 1), Some(3_000_000_000));
        assert_eq!(cmtime_ns(1_000_000_123, 1_000_000_000), Some(1_000_000_123));
        assert_eq!(cmtime_ns(1, 0), None);
        assert_eq!(cmtime_ns(-1, 600), None);
        assert_eq!(frame_interval(FrameRate::Max(30)), (1, 30));
        assert_eq!(frame_interval(FrameRate::Native), (0, 1));
        assert_eq!(frame_interval(FrameRate::OnDamage(0)), (1, 1));
    }

    #[test]
    fn fit_keeps_the_aspect_inside_the_box() {
        let native = Size::new(3840, 2160);
        assert_eq!(output_size(OutputSize::Native, native), native);
        assert_eq!(
            output_size(OutputSize::Fit(Size::new(1280, 1280)), native),
            Size::new(1280, 720)
        );
        assert_eq!(
            output_size(OutputSize::Fit(Size::new(1920, 540)), native),
            Size::new(960, 540)
        );
        assert_eq!(
            output_size(OutputSize::Scaled(Size::new(10, 10)), native),
            Size::new(10, 10)
        );
    }

    #[test]
    fn stop_codes_name_who_ended_the_stream() {
        assert_eq!(
            sck_error("start", code::USER_DECLINED),
            CaptureError::ConsentDenied
        );
        assert_eq!(
            sck_error("stream", code::USER_STOPPED),
            CaptureError::ConsentRevoked
        );
        assert_eq!(
            sck_error("stream", code::SYSTEM_STOPPED_STREAM),
            CaptureError::SourceLost
        );
        assert_eq!(
            sck_error("stream", -3811),
            CaptureError::os(ID, "stream", -3811)
        );
    }

    #[test]
    fn a_relaunch_is_needed_once_asked_and_not_granted() {
        assert_eq!(screen_recording(true, true).status, AccessStatus::Granted);
        assert_eq!(
            screen_recording(false, false).status,
            AccessStatus::NotDetermined
        );
        let asked = screen_recording(false, true);
        assert_eq!(asked.status, AccessStatus::RequiresRestart);
        assert_eq!(asked.model, ConsentModel::SystemSetting);
    }

    #[test]
    fn surfaces_get_slots_and_a_new_pool_a_new_generation() {
        let mut next = 10;
        let mut slots = SlotMap::new(3, 1);
        let mut take = |slots: &mut SlotMap, id| {
            slots.slot(id, || {
                next += 1;
                next
            })
        };
        assert_eq!(take(&mut slots, 7), (0, false));
        assert_eq!(take(&mut slots, 8), (1, false));
        assert_eq!(take(&mut slots, 7), (0, false));
        assert_eq!(take(&mut slots, 9), (2, false));
        assert_eq!(slots.generation(), 1);
        assert_eq!(take(&mut slots, 4), (0, true));
        assert_eq!(slots.generation(), 11);
        assert_eq!(take(&mut slots, 7), (1, false));
    }

    #[test]
    fn planar_audio_interleaves_into_the_asked_format() {
        let left = [0.0_f32, 1.0, -1.0];
        let right = [0.5_f32, 2.0];
        let (f32s, frames) = interleave(&[&left, &right], SampleFormat::F32);
        assert_eq!(frames, 2);
        assert_eq!(f32s.len(), 2 * 2 * 4);
        assert_eq!(f32s.get(4..8), Some(&0.5_f32.to_le_bytes()[..]));
        let (s16, _) = interleave(&[&left, &right], SampleFormat::S16);
        let samples: Vec<i16> = s16
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| i16::from_le_bytes(*b))
            .collect();
        assert_eq!(samples, vec![0, 16_384, i16::MAX, i16::MAX]);
        let converted = convert_interleaved(&[-1.0, f32::NAN], SampleFormat::S16);
        assert_eq!(
            converted,
            [(-i16::MAX).to_le_bytes(), 0_i16.to_le_bytes()].concat()
        );
    }

    #[test]
    fn source_ids_name_what_they_capture() {
        assert_eq!(
            SourceName::parse("display:69733378"),
            Some(SourceName::Display(69_733_378))
        );
        assert_eq!(SourceName::parse("window:42"), Some(SourceName::Window(42)));
        let safari = SourceName::Application {
            bundle_id: Some("com.apple.Safari".to_owned()),
            pid: None,
        };
        assert_eq!(
            SourceName::parse("app:com.apple.Safari"),
            Some(safari.clone())
        );
        let bare = SourceName::Application {
            bundle_id: None,
            pid: Some(812),
        };
        for name in [SourceName::Display(1), SourceName::Window(9), safari, bare] {
            assert_eq!(SourceName::parse(&name.value()), Some(name));
        }
        for bad in ["window:x", "monitor", "app:", "display:-1", "picker"] {
            assert_eq!(SourceName::parse(bad), None, "{bad}");
        }
    }
    #[test]
    fn audio_gaps_break_the_timeline() {
        let mut timeline = AudioTimeline::default();
        assert!(!timeline.chunk(1_000_000_000, 480, 48_000));
        // The next chunk starts 10 ms later, where this one ended.
        assert!(!timeline.chunk(1_010_000_000, 480, 48_000));
        assert!(timeline.chunk(1_500_000_000, 480, 48_000));
        assert!(!timeline.chunk(1_510_000_000, 480, 48_000));
    }
}
