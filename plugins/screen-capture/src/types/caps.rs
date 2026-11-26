//! The capability report: what each backend can do here, and how each
//! fact is known.

use istmo_macros::message;

use super::config::{CursorMode, Exhaustion, SampleFormat};
use super::consent::{AccessScope, ConsentModel};
use super::source::{BackendId, Size, SourceId, SourceKind, SourceListing};

/// An OS version (`major.minor.patch`; Windows uses `10.0.build`).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

/// Whether a compiled backend can run in this session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Availability {
    Ready,
    /// Compiled, but this session lacks what it needs ("no
    /// wlr-screencopy global", "XWayland only").
    NotOnThisSession(String),
    /// Runnable once this standing grant exists.
    NeedsAccess(AccessScope),
    OsTooOld(OsVersion),
    /// Not compiled into this build; the string is the cargo feature.
    NotCompiled(String),
    /// The app's `CapturePolicy` excludes it.
    PolicyDenied(String),
}

impl Availability {
    #[must_use]
    pub const fn is_ready(&self) -> bool {
        matches!(self, Self::Ready)
    }
}

/// One entry of `backends()`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendStatus {
    pub backend: BackendId,
    pub availability: Availability,
}

/// What `capabilities` should describe.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct CapabilityQuery {
    /// One backend; `None` = every runnable backend.
    pub backend: Option<BackendId>,
    /// Restrict the report to one source kind.
    pub kind: Option<SourceKind>,
    /// Describe this concrete source.
    pub source: Option<SourceId>,
}

/// How a fact in the report is known.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Provenance {
    /// Asked from the running system, without a prompt.
    Probe,
    /// From the platform tables (OS / protocol version); not checked on
    /// this machine.
    Table,
    /// Known only after `open` (consent, a picker choice or the first
    /// frame). The section then holds the backend's theoretical set,
    /// never an empty list that reads as "nothing".
    UnknownUntilNegotiated,
}

/// Per-section provenance of a [`BackendCaps`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CapsProvenance {
    pub source_kinds: Provenance,
    pub listing: Provenance,
    pub video: Provenance,
    pub cursor: Provenance,
    pub damage: Provenance,
    pub colour: Provenance,
    pub audio: Provenance,
    pub producer: Provenance,
    pub sync: Provenance,
    pub persistence: Provenance,
}

impl CapsProvenance {
    /// Every section from the tables.
    pub const TABLE: Self = Self::all(Provenance::Table);

    /// Every section set to `provenance`.
    #[must_use]
    pub const fn all(provenance: Provenance) -> Self {
        Self {
            source_kinds: provenance,
            listing: provenance,
            video: provenance,
            cursor: provenance,
            damage: provenance,
            colour: provenance,
            audio: provenance,
            producer: provenance,
            sync: provenance,
            persistence: provenance,
        }
    }
}

/// Pixel formats, named after their byte order in memory (`Bgra8` is
/// DRM `ARGB8888`, `B, G, R, A` bytes on a little-endian machine).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    Bgra8,
    Bgrx8,
    Rgba8,
    Rgbx8,
    /// 10-bit `XRGB2101010` (DRM `XR30`).
    Xr30,
    /// 10-bit `XBGR2101010` (DRM `XB30`).
    Xb30,
    /// 10-bit `ARGB2101010` (DRM `AR30`).
    Ar30,
    /// Half-float RGBA (scRGB / HDR).
    Rgba16f,
    /// 8-bit 4:2:0 biplanar.
    Nv12,
    /// 10-bit 4:2:0 biplanar.
    P010,
}

impl PixelFormat {
    /// Bytes per pixel of the first plane.
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Bgra8
            | Self::Bgrx8
            | Self::Rgba8
            | Self::Rgbx8
            | Self::Xr30
            | Self::Xb30
            | Self::Ar30 => 4,
            Self::Rgba16f => 8,
            Self::Nv12 => 1,
            Self::P010 => 2,
        }
    }

    /// Whether the format carries meaningful alpha.
    #[must_use]
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Bgra8 | Self::Rgba8 | Self::Ar30 | Self::Rgba16f)
    }

    /// The DRM fourcc (`drm_fourcc.h`).
    #[must_use]
    pub const fn drm_fourcc(self) -> u32 {
        const fn fourcc(code: [u8; 4]) -> u32 {
            u32::from_le_bytes(code)
        }
        match self {
            Self::Bgra8 => fourcc(*b"AR24"),
            Self::Bgrx8 => fourcc(*b"XR24"),
            Self::Rgba8 => fourcc(*b"AB24"),
            Self::Rgbx8 => fourcc(*b"XB24"),
            Self::Xr30 => fourcc(*b"XR30"),
            Self::Xb30 => fourcc(*b"XB30"),
            Self::Ar30 => fourcc(*b"AR30"),
            Self::Rgba16f => fourcc(*b"AB4H"),
            Self::Nv12 => fourcc(*b"NV12"),
            Self::P010 => fourcc(*b"P010"),
        }
    }

    /// The format a DRM fourcc names, if the plugin models it.
    #[must_use]
    pub const fn from_drm_fourcc(code: u32) -> Option<Self> {
        let all = [
            Self::Bgra8,
            Self::Bgrx8,
            Self::Rgba8,
            Self::Rgbx8,
            Self::Xr30,
            Self::Xb30,
            Self::Ar30,
            Self::Rgba16f,
            Self::Nv12,
            Self::P010,
        ];
        let mut i = 0;
        while i < all.len() {
            if all[i].drm_fourcc() == code {
                return Some(all[i]);
            }
            i += 1;
        }
        None
    }
}

/// Where a frame's pixels live. Wire-encodable sibling of
/// `istmo_core::MemoryKind` (which is not a wire type and has no web
/// variant); the different name keeps both glob-importable.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BufferMemory {
    Cpu,
    DmaBuf,
    D3d11Texture,
    IoSurface,
    CvPixelBuffer,
    HardwareBuffer,
    WebVideoFrame,
}

impl BufferMemory {
    /// The overlap with `istmo_core::MemoryKind`; `None` for kinds the
    /// plugin never produces.
    #[must_use]
    pub const fn from_memory_kind(kind: istmo_core::MemoryKind) -> Option<Self> {
        use istmo_core::MemoryKind;
        Some(match kind {
            MemoryKind::Cpu => Self::Cpu,
            MemoryKind::DmaBuf => Self::DmaBuf,
            MemoryKind::D3d11Texture => Self::D3d11Texture,
            MemoryKind::IoSurface => Self::IoSurface,
            MemoryKind::CvPixelBuffer => Self::CvPixelBuffer,
            MemoryKind::HardwareBuffer => Self::HardwareBuffer,
            MemoryKind::VideoFrame => Self::WebVideoFrame,
            _ => return None,
        })
    }
}

/// A copy the plugin makes. Each one is declared: in the capability
/// report, in `NegotiatedStream::copies` (every frame) or in
/// `FrameMeta::copied` (this frame).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CopyStage {
    /// DXGI: one acquired frame at a time, copied into the pool.
    DxgiMandatory,
    /// Copied onto the consumer's GPU.
    CrossDevice,
    /// GPU colour conversion.
    FormatConversion,
    /// Multi-monitor composite blit.
    Composite,
    /// `OrientationHandling::Upright`.
    OrientationUpright,
    /// `Exhaustion::CopyToCpu`.
    ExhaustionReadback,
    /// `wl_shm` / `MemFd` / XShm: the producer copied into CPU memory.
    ShmTransport,
    /// A live buffer (X11 DRI3, KMS) copied so it stops changing.
    LiveBufferSnapshot,
    /// `VideoFrame.copyTo` into wasm memory.
    WebCopyTo,
    /// Android: one capture drawn into the preview and the sink.
    PreviewFanOut,
    /// iOS legacy extension ring.
    LegacyRing,
}

/// One format a backend can deliver.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FormatCaps {
    pub format: PixelFormat,
    pub memory: BufferMemory,
    /// DMA-BUF only; LINEAR only when it is really allocatable.
    pub modifiers: Vec<u64>,
    /// Copies needed to deliver this entry (empty = none).
    pub copies: Vec<CopyStage>,
}

/// Who scales to `OutputSize::Scaled` / `Fit`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Scaling {
    /// Nobody: native size only.
    Unsupported,
    /// The plugin scales (a declared copy).
    Backend,
    /// The OS scales at the source.
    Native,
}

/// How a region crop is delivered.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RegionCrop {
    /// The OS crops (portal `VideoCrop`, SCK `sourceRect`).
    Native,
    /// Full buffers; the crop is `FrameMeta::content_rect`.
    InMeta,
}

/// One composite mode's support.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompositeSupport {
    Supported,
    Unsupported,
    GpuOnly,
    CpuOnly,
}

/// Multi-monitor composite support.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CompositeCaps {
    pub screenshot: CompositeSupport,
    pub streaming: CompositeSupport,
}

/// Damage reporting.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DamageSupport {
    Unsupported,
    /// Whole-frame changed / unchanged only.
    Frame,
    /// Damage rectangles.
    Rects,
}

/// How buffer orientation reaches the consumer.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrientationSupport {
    /// Buffers as scanned out, the fix-up in `FrameMeta::orientation`.
    InMeta,
    /// Buffers arrive upright.
    AlreadyApplied,
    /// Orientation is unknown.
    Unsupported,
}

/// Colour primaries / encoding.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourSpace {
    Srgb,
    DisplayP3,
    Bt709,
    Bt2020,
    /// Linear extended sRGB (Windows / macOS HDR).
    ScRgb,
    Unknown,
}

/// Transfer function.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransferFunction {
    Srgb,
    Linear,
    Pq,
    Hlg,
    Bt709,
    Unknown,
}

/// Quantisation range.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourRange {
    Full,
    Limited,
}

/// A frame's colour description.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Colour {
    pub space: ColourSpace,
    pub transfer: TransferFunction,
    pub range: ColourRange,
}

impl Colour {
    /// sRGB, full range: what every SDR desktop backend delivers.
    pub const SRGB: Self = Self {
        space: ColourSpace::Srgb,
        transfer: TransferFunction::Srgb,
        range: ColourRange::Full,
    };
}

/// Colour capabilities.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ColourCaps {
    pub colours: Vec<Colour>,
    pub hdr: bool,
}

/// Kinds of audio source a backend can capture.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AudioSourceKind {
    System,
    Application,
    ExcludeSelf,
    Node,
}

/// Audio capture capabilities.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AudioCaps {
    pub sources: Vec<AudioSourceKind>,
    pub formats: Vec<SampleFormat>,
    /// Capture can deliver silence the plugin cannot detect (Android
    /// opt-outs).
    pub may_be_silent: bool,
}

impl AudioCaps {
    /// No audio.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            sources: Vec::new(),
            formats: Vec::new(),
            may_be_silent: false,
        }
    }
}

/// The GPU that produced a buffer.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GpuIdentity {
    /// Linux DRM device number (`dev_t` of the render node).
    Drm(u64),
    /// Windows adapter LUID.
    Luid(u64),
    /// macOS / iOS `MTLDevice.registryID`.
    RegistryId(u64),
}

/// How a frame's writes are synchronised. Timeline sync never reaches
/// consumers: the producer waits and says `Ready`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SyncKind {
    /// Writing finished before the frame was delivered.
    Ready,
    /// DMA-BUF implicit fences (best effort on NVIDIA).
    Implicit,
}

/// Whether consent survives a session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Persistence {
    Unsupported,
    /// A restore token the app stores (portal v4+).
    Token,
    /// A standing system setting (TCC, borderless).
    SystemSetting,
}

/// Buffer pool limits.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PoolCaps {
    pub min_depth: u8,
    pub max_depth: u8,
    pub default_depth: u8,
    pub exhaustion: Vec<Exhaustion>,
}

/// What happens to protected content.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProtectedContent {
    /// The OS says which frames are masked.
    Reported,
    /// Masked, and the plugin cannot tell.
    MaskedUndetectable,
    /// Protected content comes out blank.
    Blank,
}

/// Window capture behaviour.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts per platform"
)]
pub struct WindowCaptureCaps {
    pub occluded: bool,
    pub minimised: bool,
    pub child_popups: bool,
    /// The OS draws a capture border.
    pub border: bool,
}

/// A typed caveat.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CapabilityNote {
    /// NVIDIA attaches no implicit fences; `Implicit` frames are best
    /// effort there.
    NvidiaImplicitSyncBestEffort,
    /// The compositor is not known to wait for its render before
    /// `ready`; frames leave as `Implicit`.
    ReadyNotKnownToWait,
    /// Buffers are live (DRI3, KMS): their content keeps changing.
    LiveBuffer,
    /// KMS ignores every compositor-side protection.
    KmsBypassesCompositor,
    /// No `linux-dmabuf` feedback: DMA-BUF tiers are unavailable, only
    /// shared memory.
    NoDmabufFeedback,
    /// GBM could not open the compositor's render node.
    NoGbmDevice,
    /// The session runs under XWayland; X11 capture would only see X
    /// clients.
    XWaylandOnly,
}

/// Everything one backend reports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendCaps {
    pub backend: BackendId,
    pub source_kinds: Vec<SourceKind>,
    pub listing: SourceListing,
    pub consent: ConsentModel,
    pub video: Vec<FormatCaps>,
    pub max_fps: Option<u32>,
    pub min_interval_us: Option<u32>,
    pub max_size: Option<Size>,
    pub scaling: Scaling,
    pub region: RegionCrop,
    pub composite: CompositeCaps,
    pub cursor: Vec<CursorMode>,
    pub damage: DamageSupport,
    pub orientation: OrientationSupport,
    pub colour: ColourCaps,
    pub audio: AudioCaps,
    pub producer: Option<GpuIdentity>,
    pub sync: Vec<SyncKind>,
    pub persistence: Persistence,
    pub concurrency: Option<u32>,
    pub pool: PoolCaps,
    pub copies: Vec<CopyStage>,
    pub protected_content: ProtectedContent,
    pub window: WindowCaptureCaps,
    pub provenance: CapsProvenance,
    pub notes: Vec<CapabilityNote>,
}

/// The answer to `capabilities`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CapabilityReport {
    pub backends: Vec<BackendCaps>,
}

impl BackendCaps {
    /// A report with nothing claimed: monitors only, enumerable, no
    /// consent, no formats, every section from the tables. Backends
    /// start here and fill in what they know.
    #[must_use]
    pub fn new(backend: BackendId) -> Self {
        Self {
            backend,
            source_kinds: vec![SourceKind::Monitor],
            listing: SourceListing::Enumerable,
            consent: ConsentModel::NoneNeeded,
            video: Vec::new(),
            max_fps: None,
            min_interval_us: None,
            max_size: None,
            scaling: Scaling::Unsupported,
            region: RegionCrop::InMeta,
            composite: CompositeCaps {
                screenshot: CompositeSupport::CpuOnly,
                streaming: CompositeSupport::Unsupported,
            },
            cursor: vec![CursorMode::Hidden],
            damage: DamageSupport::Unsupported,
            orientation: OrientationSupport::InMeta,
            colour: ColourCaps {
                colours: vec![Colour::SRGB],
                hdr: false,
            },
            audio: AudioCaps::none(),
            producer: None,
            sync: vec![SyncKind::Ready],
            persistence: Persistence::Unsupported,
            concurrency: None,
            pool: PoolCaps {
                min_depth: 2,
                max_depth: 8,
                default_depth: 4,
                exhaustion: vec![Exhaustion::Stall, Exhaustion::DropNewest],
            },
            copies: Vec::new(),
            protected_content: ProtectedContent::Blank,
            window: WindowCaptureCaps::default(),
            provenance: CapsProvenance::TABLE,
            notes: Vec::new(),
        }
    }
}
