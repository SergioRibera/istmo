//! Session, screenshot and preview configuration.
//!
//! Wire types are plain structs. Rust gets `Default` and builders;
//! Kotlin and Swift get `ScreenCaptureDefaults` helpers shipped in the
//! plugin's native sources.

use istmo_core::NativeHandleId;
use istmo_macros::message;

use super::caps::{BufferMemory, Colour, CopyStage, GpuIdentity, PixelFormat};
use super::consent::ConsentConfig;
use super::source::{
    BackendId, PickerOptions, Rect, RegionSelector, RegionSpace, Size, SourceId, SourceSelector,
};

/// How `Auto` orders the runnable backends. It reorders, never filters.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum AutoPreference {
    #[default]
    ZeroCopy,
    LowLatency,
    Compatibility,
}

/// `Auto` options.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AutoSelect {
    pub prefer: AutoPreference,
    pub exclude: Vec<BackendId>,
}

/// Which backend serves a call.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BackendSelection {
    /// The first runnable backend, in platform order, whose known facts
    /// meet every requirement. `ISTMO_SCREEN_CAPTURE_BACKEND` replaces it.
    Auto(AutoSelect),
    /// This backend or its error.
    Fixed(BackendId),
    /// These, in order; `NoBackend` when none works.
    Ordered(Vec<BackendId>),
}

impl Default for BackendSelection {
    fn default() -> Self {
        Self::Auto(AutoSelect::default())
    }
}

/// One acceptable format.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FormatChoice {
    pub format: PixelFormat,
    pub memory: BufferMemory,
    /// DMA-BUF modifiers in preference order; empty = any the producer
    /// offers (never the implicit `INVALID` modifier unless listed).
    pub modifiers: Vec<u64>,
}

impl FormatChoice {
    /// A CPU-memory choice.
    #[must_use]
    pub const fn cpu(format: PixelFormat) -> Self {
        Self {
            format,
            memory: BufferMemory::Cpu,
            modifiers: Vec::new(),
        }
    }

    /// A DMA-BUF choice with these modifiers (empty = any).
    #[must_use]
    pub const fn dmabuf(format: PixelFormat, modifiers: Vec<u64>) -> Self {
        Self {
            format,
            memory: BufferMemory::DmaBuf,
            modifiers,
        }
    }
}

/// Equally acceptable choices plus the copies tolerated to get them.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FormatTier {
    /// In preference order.
    pub choices: Vec<FormatChoice>,
    /// Copies acceptable in this tier (empty = must be copy-free).
    pub allows_copy: Vec<CopyStage>,
}

impl FormatTier {
    /// A copy-free tier.
    #[must_use]
    pub const fn new(choices: Vec<FormatChoice>) -> Self {
        Self {
            choices,
            allows_copy: Vec::new(),
        }
    }

    /// The same tier, also accepting `copy`.
    #[must_use]
    pub fn allowing(mut self, copy: CopyStage) -> Self {
        if !self.allows_copy.contains(&copy) {
            self.allows_copy.push(copy);
        }
        self
    }

    /// A CPU tier of the 8-bit RGB formats, accepting shared-memory
    /// transport.
    #[must_use]
    pub fn cpu_rgb() -> Self {
        Self::new(
            [
                PixelFormat::Bgra8,
                PixelFormat::Bgrx8,
                PixelFormat::Rgba8,
                PixelFormat::Rgbx8,
            ]
            .into_iter()
            .map(FormatChoice::cpu)
            .collect(),
        )
        .allowing(CopyStage::ShmTransport)
    }
}

/// Ordered tiers, best first. The union of all tiers is the
/// requirement, their order the preference; empty = native formats, any
/// memory.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct FormatRequest {
    pub tiers: Vec<FormatTier>,
}

impl FormatRequest {
    /// A single CPU tier (what screenshots read as pixels use).
    #[must_use]
    pub fn cpu() -> Self {
        Self {
            tiers: vec![FormatTier::cpu_rgb()],
        }
    }

    /// Whether any tier lists CPU memory.
    #[must_use]
    pub fn has_cpu_tier(&self) -> bool {
        self.tiers
            .iter()
            .any(|t| t.choices.iter().any(|c| c.memory == BufferMemory::Cpu))
    }
}

/// Frame-rate policy.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FrameRate {
    /// Every frame the producer delivers.
    Native,
    /// At most `n` per second; frames inside a period are deferred and
    /// the newest is delivered at its end, with the skipped damage merged.
    Max(u32),
    /// Exactly `n` per second; an idle source repeats its last frame
    /// (same buffer, `FrameMeta::repeat`).
    Exact(u32),
    /// Only when content changed, at most `n` per second.
    OnDamage(u32),
}

impl Default for FrameRate {
    fn default() -> Self {
        Self::Max(60)
    }
}

/// Output size.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OutputSize {
    #[default]
    Native,
    Scaled(Size),
    /// Largest size inside this box, keeping the aspect ratio.
    Fit(Size),
}

/// Cursor rendering.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CursorMode {
    Hidden,
    /// Drawn into the frame.
    Embedded,
    /// Position (and image where available) in `FrameMeta::cursor`.
    Metadata,
}

/// How hard a preference is.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Strength {
    #[default]
    Ignore,
    Prefer,
    /// Unsatisfiable when not met.
    Require,
}

/// Cursor request.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CursorRequest {
    pub mode: CursorMode,
    pub strength: Strength,
}

impl Default for CursorRequest {
    fn default() -> Self {
        Self {
            mode: CursorMode::Embedded,
            strength: Strength::Prefer,
        }
    }
}

/// HDR policy. The plugin never tone-maps.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HdrPolicy {
    #[default]
    Sdr,
    PreferHdr,
    RequireHdr,
}

/// What to do when the consumer holds every buffer it may hold.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Exhaustion {
    /// Stop pulling until a buffer returns; the next frame is fresh and
    /// carries `skipped_before`.
    #[default]
    Stall,
    /// Keep the producer running, discard frames until a buffer returns.
    DropNewest,
    /// Read the frame back into CPU memory and deliver that
    /// (`CopyStage::ExhaustionReadback`).
    CopyToCpu,
}

/// Which GPU should produce the buffers.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GpuPreference {
    #[default]
    Auto,
    Same(GpuIdentity),
    Discrete,
    Integrated,
}

/// Who turns the buffer upright.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum OrientationHandling {
    /// The consumer, from `FrameMeta::orientation`.
    #[default]
    InMeta,
    /// The plugin (`CopyStage::OrientationUpright`).
    Upright,
}

/// Video options of a session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VideoConfig {
    pub formats: FormatRequest,
    pub fps: FrameRate,
    pub size: OutputSize,
    pub cursor: CursorRequest,
    /// Damage rectangles.
    pub damage: Strength,
    pub hdr: HdrPolicy,
    /// 0 = backend default. Clamped per backend and reported.
    pub pool_depth: u8,
    pub exhaustion: Exhaustion,
    pub gpu: GpuPreference,
    pub orientation: OrientationHandling,
    /// Windows / apps to leave out where the backend can.
    pub exclude: Vec<SourceId>,
    pub exclude_own_process: bool,
    /// Accept live (non-snapshot) buffers: X11 DRI3, KMS.
    pub live_buffers: bool,
}

impl Default for VideoConfig {
    fn default() -> Self {
        Self {
            formats: FormatRequest::default(),
            fps: FrameRate::default(),
            size: OutputSize::Native,
            cursor: CursorRequest::default(),
            damage: Strength::Ignore,
            hdr: HdrPolicy::Sdr,
            pool_depth: 0,
            exhaustion: Exhaustion::Stall,
            gpu: GpuPreference::Auto,
            orientation: OrientationHandling::InMeta,
            exclude: Vec::new(),
            exclude_own_process: false,
            live_buffers: false,
        }
    }
}

/// Interleaved PCM sample format.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SampleFormat {
    S16,
    #[default]
    F32,
}

impl SampleFormat {
    /// Bytes per sample (one channel).
    #[must_use]
    pub const fn bytes(self) -> u32 {
        match self {
            Self::S16 => 2,
            Self::F32 => 4,
        }
    }
}

/// Which audio to capture.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum AudioSource {
    /// Everything the system plays.
    #[default]
    System,
    /// One application (by app id / binary name).
    Application(String),
    /// Everything except this process.
    ExcludeSelf,
    /// A PipeWire node id.
    Node(u32),
}

/// Audio options of a session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct AudioConfig {
    pub source: AudioSource,
    /// `None` = the source's rate.
    pub rate: Option<u32>,
    /// `None` = the source's channel count.
    pub channels: Option<u8>,
    pub format: SampleFormat,
}

/// Composite scale.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompositeScale {
    /// The largest monitor scale, so no monitor is downsampled.
    #[default]
    HighestScale,
    /// 1.0.
    Logical,
    /// This scale (1000 = 1.0).
    Permille(u32),
}

/// What fills the gaps of a non-rectangular layout.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompositeFill {
    /// Needs an alpha format; black otherwise (and reported).
    #[default]
    Transparent,
    Black,
}

/// Who composes.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CompositeEngine {
    #[default]
    Auto,
    Gpu,
    Cpu,
}

/// Multi-monitor composite options.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct CompositeConfig {
    pub scale: CompositeScale,
    pub fill: CompositeFill,
    pub engine: CompositeEngine,
}

/// Which PipeWire daemon the portal backend talks to.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PipeWireConnection {
    /// The portal's `OpenPipeWireRemote` fd (works in Flatpak).
    #[default]
    PortalRemote,
    /// The user's session daemon.
    SessionDaemon,
}

/// Linux-only options.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct LinuxOptions {
    pub pipewire: PipeWireConnection,
}

/// Windows capture border.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Borderless {
    #[default]
    Off,
    Prefer,
    Require,
}

/// Windows-only options.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct WindowsOptions {
    pub borderless: Borderless,
}

/// A live session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SessionConfig {
    pub source: SourceSelector,
    pub backend: BackendSelection,
    pub video: VideoConfig,
    pub audio: Option<AudioConfig>,
    pub consent: ConsentConfig,
    pub composite: CompositeConfig,
    pub linux: LinuxOptions,
    pub windows: WindowsOptions,
}

impl SessionConfig {
    /// A session on `source`, defaults otherwise.
    #[must_use]
    pub fn of(source: SourceSelector) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    /// A session on the monitor `id` (from `sources`). An id that names a
    /// window or an application is refused with
    /// `Unsatisfiable(Kind(..))`.
    #[must_use]
    pub fn monitor(id: SourceId) -> Self {
        Self::of(SourceSelector::Monitors(vec![id]))
    }

    /// A session on the primary monitor (the default source).
    #[must_use]
    pub fn primary_monitor() -> Self {
        Self::of(SourceSelector::PrimaryMonitor)
    }

    /// A session on whatever the person picks.
    #[must_use]
    pub fn picker() -> Self {
        let mut config = Self::of(SourceSelector::Picker(PickerOptions::default()));
        config.consent.allow_picker = true;
        config
    }

    /// A session on a global-space region.
    #[must_use]
    pub fn region(rect: Rect) -> Self {
        Self::of(SourceSelector::Region(RegionSelector {
            rect,
            space: RegionSpace::Global,
        }))
    }

    #[must_use]
    pub const fn fps(mut self, fps: FrameRate) -> Self {
        self.video.fps = fps;
        self
    }

    /// `Auto` preferring zero-copy backends.
    #[must_use]
    pub fn prefer_zero_copy(mut self) -> Self {
        self.backend = BackendSelection::Auto(AutoSelect {
            prefer: AutoPreference::ZeroCopy,
            exclude: Vec::new(),
        });
        self
    }

    #[must_use]
    pub fn backend(mut self, backend: BackendSelection) -> Self {
        self.backend = backend;
        self
    }

    #[must_use]
    pub fn formats(mut self, formats: FormatRequest) -> Self {
        self.video.formats = formats;
        self
    }

    #[must_use]
    pub const fn cursor(mut self, cursor: CursorRequest) -> Self {
        self.video.cursor = cursor;
        self
    }

    #[must_use]
    pub const fn exhaustion(mut self, exhaustion: Exhaustion) -> Self {
        self.video.exhaustion = exhaustion;
        self
    }

    #[must_use]
    pub const fn pool_depth(mut self, depth: u8) -> Self {
        self.video.pool_depth = depth;
        self
    }

    #[must_use]
    pub fn audio(mut self, audio: AudioConfig) -> Self {
        self.audio = Some(audio);
        self
    }

    #[must_use]
    pub fn source(mut self, source: SourceSelector) -> Self {
        self.source = source;
        self
    }

    /// CPU frames (packed RGB in shared memory, the producer's copy
    /// allowed): what a consumer that reads `as_bytes` wants. The default
    /// (an empty request) takes the backend's native memory, often a
    /// DMA-BUF that only a GPU consumer can read.
    #[must_use]
    pub fn cpu_frames(mut self) -> Self {
        self.video.formats = FormatRequest::cpu();
        self
    }

    #[must_use]
    pub const fn size(mut self, size: OutputSize) -> Self {
        self.video.size = size;
        self
    }

    #[must_use]
    pub const fn damage(mut self, damage: Strength) -> Self {
        self.video.damage = damage;
        self
    }

    #[must_use]
    pub const fn orientation(mut self, orientation: OrientationHandling) -> Self {
        self.video.orientation = orientation;
        self
    }

    #[must_use]
    pub const fn gpu(mut self, gpu: GpuPreference) -> Self {
        self.video.gpu = gpu;
        self
    }

    #[must_use]
    pub fn consent(mut self, consent: ConsentConfig) -> Self {
        self.consent = consent;
        self
    }
}

/// A change to a running session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SessionUpdate {
    Fps(FrameRate),
    Cursor(CursorRequest),
    Region(RegionSelector),
    Formats(FormatRequest),
    Pause,
    Resume,
}

/// One screenshot.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ScreenshotRequest {
    pub source: SourceSelector,
    pub backend: BackendSelection,
    /// Empty = a CPU tier first.
    pub formats: FormatRequest,
    pub cursor: CursorRequest,
    pub hdr: HdrPolicy,
    /// Several monitors: one composite (`true`) or one frame each.
    pub composite: bool,
    pub composite_config: CompositeConfig,
    pub consent: ConsentConfig,
}

impl Default for ScreenshotRequest {
    fn default() -> Self {
        Self {
            source: SourceSelector::PrimaryMonitor,
            backend: BackendSelection::default(),
            formats: FormatRequest::default(),
            cursor: CursorRequest::default(),
            hdr: HdrPolicy::Sdr,
            composite: true,
            composite_config: CompositeConfig::default(),
            consent: ConsentConfig::default(),
        }
    }
}

impl ScreenshotRequest {
    #[must_use]
    pub fn of(source: SourceSelector) -> Self {
        Self {
            source,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn formats(mut self, formats: FormatRequest) -> Self {
        self.formats = formats;
        self
    }

    #[must_use]
    pub const fn cursor(mut self, cursor: CursorRequest) -> Self {
        self.cursor = cursor;
        self
    }

    #[must_use]
    pub fn consent(mut self, consent: ConsentConfig) -> Self {
        self.consent = consent;
        self
    }

    /// Several monitors as one composite (`true`, the default) or one
    /// frame each.
    #[must_use]
    pub const fn composite(mut self, composite: bool) -> Self {
        self.composite = composite;
        self
    }

    #[must_use]
    pub fn backend(mut self, backend: BackendSelection) -> Self {
        self.backend = backend;
        self
    }
}

/// CPU pixel layouts `screenshot_pixels` can return.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum CpuPixelFormat {
    #[default]
    Rgba8,
    Bgra8,
    /// Half-float RGBA; only from a half-float source (never tone-mapped).
    Rgba16f,
}

impl CpuPixelFormat {
    #[must_use]
    pub const fn bytes_per_pixel(self) -> u32 {
        match self {
            Self::Rgba8 | Self::Bgra8 => 4,
            Self::Rgba16f => 8,
        }
    }
}

/// How `screenshot_pixels` lays out its bytes.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PixelLayout {
    pub format: CpuPixelFormat,
    /// Apply the source orientation.
    pub upright: bool,
    /// `stride = width × bytes per pixel`.
    pub tight: bool,
}

impl Default for PixelLayout {
    fn default() -> Self {
        Self {
            format: CpuPixelFormat::Rgba8,
            upright: true,
            tight: true,
        }
    }
}

/// Raw pixels. Never encoded.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RawImage {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub format: CpuPixelFormat,
    pub colour: Colour,
    pub source: SourceId,
    pub bytes: Vec<u8>,
}

impl std::fmt::Debug for RawImage {
    // The pixels would drown every log line they appear in.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RawImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("stride", &self.stride)
            .field("format", &self.format)
            .field("colour", &self.colour)
            .field("source", &self.source)
            .field("bytes", &self.bytes.len())
            .finish()
    }
}

/// What kind of platform surface a preview renders into.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreviewSurfaceKind {
    AndroidSurface,
    AvSampleBufferDisplayLayer,
    HtmlVideoElement,
}

/// How the capture fills the surface.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PreviewFit {
    #[default]
    Fit,
    Fill,
    Stretch,
}

/// A platform surface registered in the runtime's handle table.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PreviewTarget {
    pub surface: NativeHandleId,
    pub kind: PreviewSurfaceKind,
    pub fit: PreviewFit,
}
