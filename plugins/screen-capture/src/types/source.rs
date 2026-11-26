//! Backends, sources and how a caller names what to capture.

use std::fmt;
use std::str::FromStr;

use istmo_macros::message;

/// One capture backend. The id is stable: it appears in capability
/// reports, errors, restore tokens and the
/// `ISTMO_SCREEN_CAPTURE_BACKEND` override (as [`BackendId::as_str`]).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BackendId {
    /// xdg-desktop-portal ScreenCast + PipeWire.
    LinuxPortal,
    /// `ext-image-copy-capture-v1`.
    LinuxExtImageCopy,
    /// `zwlr_screencopy_manager_v1`.
    LinuxWlrScreencopy,
    /// X11 (XShm, Composite, XDamage, XFixes).
    LinuxX11,
    /// DRM / KMS framebuffer export (double opt-in).
    LinuxKms,
    /// Windows.Graphics.Capture.
    WindowsWgc,
    /// DXGI Desktop Duplication.
    WindowsDxgi,
    /// GDI `BitBlt` / `PrintWindow` (screenshots only).
    WindowsGdi,
    /// macOS ScreenCaptureKit.
    MacosSck,
    /// iOS / iPadOS 27 ScreenCaptureKit (picker).
    IosSck,
    /// iOS < 27 ReplayKit in-app capture.
    IosReplayKit,
    /// Android MediaProjection.
    AndroidProjection,
    /// Browser `getDisplayMedia`.
    Web,
}

impl BackendId {
    /// Every backend id, in declaration order.
    pub const ALL: [Self; 13] = [
        Self::LinuxPortal,
        Self::LinuxExtImageCopy,
        Self::LinuxWlrScreencopy,
        Self::LinuxX11,
        Self::LinuxKms,
        Self::WindowsWgc,
        Self::WindowsDxgi,
        Self::WindowsGdi,
        Self::MacosSck,
        Self::IosSck,
        Self::IosReplayKit,
        Self::AndroidProjection,
        Self::Web,
    ];

    /// The id as written in the environment override and in logs; it is
    /// also the name of the cargo feature that compiles the backend.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LinuxPortal => "linux-portal",
            Self::LinuxExtImageCopy => "linux-ext-image-copy",
            Self::LinuxWlrScreencopy => "linux-wlr-screencopy",
            Self::LinuxX11 => "linux-x11",
            Self::LinuxKms => "linux-kms",
            Self::WindowsWgc => "windows-wgc",
            Self::WindowsDxgi => "windows-dxgi",
            Self::WindowsGdi => "windows-gdi",
            Self::MacosSck => "macos-sck",
            Self::IosSck => "ios-sck",
            Self::IosReplayKit => "ios-replaykit",
            Self::AndroidProjection => "android-projection",
            Self::Web => "web",
        }
    }
}

impl fmt::Display for BackendId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A string that names no backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownBackend(pub String);

impl fmt::Display for UnknownBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "`{}` names no screen-capture backend", self.0)
    }
}

impl std::error::Error for UnknownBackend {}

impl FromStr for BackendId {
    type Err = UnknownBackend;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let wanted = s.trim();
        Self::ALL
            .into_iter()
            .find(|id| id.as_str().eq_ignore_ascii_case(wanted))
            .ok_or_else(|| UnknownBackend(wanted.to_owned()))
    }
}

/// What a source is.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceKind {
    Monitor,
    Window,
    Application,
}

/// A source id: opaque, and only meaningful to the backend that issued
/// it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceId {
    pub backend: BackendId,
    pub value: String,
}

impl SourceId {
    #[must_use]
    pub fn new(backend: BackendId, value: impl Into<String>) -> Self {
        Self {
            backend,
            value: value.into(),
        }
    }
}

/// A point in pixels (logical or physical, as the field says).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Point {
    pub x: i32,
    pub y: i32,
}

/// A size in pixels.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Size {
    pub width: u32,
    pub height: u32,
}

impl Size {
    #[must_use]
    pub const fn new(width: u32, height: u32) -> Self {
        Self { width, height }
    }

    /// Width and height swapped (a quarter turn).
    #[must_use]
    pub const fn transposed(self) -> Self {
        Self {
            width: self.height,
            height: self.width,
        }
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }
}

/// An axis-aligned rectangle.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: i32, y: i32, width: u32, height: u32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// `(0, 0, size)`.
    #[must_use]
    pub const fn from_size(size: Size) -> Self {
        Self::new(0, 0, size.width, size.height)
    }

    #[must_use]
    pub const fn size(self) -> Size {
        Size::new(self.width, self.height)
    }

    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.width == 0 || self.height == 0
    }

    /// Exclusive right edge, widened so it cannot overflow.
    #[must_use]
    pub const fn right(self) -> i64 {
        self.x as i64 + self.width as i64
    }

    /// Exclusive bottom edge, widened so it cannot overflow.
    #[must_use]
    pub const fn bottom(self) -> i64 {
        self.y as i64 + self.height as i64
    }

    /// The overlap of two rectangles, `None` when they do not overlap.
    #[must_use]
    pub fn intersect(self, other: Self) -> Option<Self> {
        let x0 = i64::from(self.x).max(i64::from(other.x));
        let y0 = i64::from(self.y).max(i64::from(other.y));
        let x1 = self.right().min(other.right());
        let y1 = self.bottom().min(other.bottom());
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        Some(Self::new(
            i32::try_from(x0).ok()?,
            i32::try_from(y0).ok()?,
            u32::try_from(x1 - x0).ok()?,
            u32::try_from(y1 - y0).ok()?,
        ))
    }

    /// The smallest rectangle holding both.
    #[must_use]
    pub fn union(self, other: Self) -> Self {
        if self.is_empty() {
            return other;
        }
        if other.is_empty() {
            return self;
        }
        let x0 = self.x.min(other.x);
        let y0 = self.y.min(other.y);
        let x1 = self.right().max(other.right());
        let y1 = self.bottom().max(other.bottom());
        Self::new(
            x0,
            y0,
            u32::try_from(x1 - i64::from(x0)).unwrap_or(u32::MAX),
            u32::try_from(y1 - i64::from(y0)).unwrap_or(u32::MAX),
        )
    }

    /// Whether `other` lies entirely inside `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        other.x >= self.x
            && other.y >= self.y
            && other.right() <= self.right()
            && other.bottom() <= self.bottom()
    }

    /// The rectangle moved by `(-dx, -dy)`: into the space whose origin is
    /// `(dx, dy)`.
    #[must_use]
    pub const fn relative_to(self, dx: i32, dy: i32) -> Self {
        Self::new(
            self.x.saturating_sub(dx),
            self.y.saturating_sub(dy),
            self.width,
            self.height,
        )
    }
}

/// A clockwise quarter turn.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Rotation {
    #[default]
    R0,
    R90,
    R180,
    R270,
}

impl Rotation {
    /// Number of clockwise quarter turns.
    #[must_use]
    pub const fn quarter_turns(self) -> u8 {
        match self {
            Self::R0 => 0,
            Self::R90 => 1,
            Self::R180 => 2,
            Self::R270 => 3,
        }
    }

    #[must_use]
    pub const fn from_quarter_turns(turns: u8) -> Self {
        match turns % 4 {
            0 => Self::R0,
            1 => Self::R90,
            2 => Self::R180,
            _ => Self::R270,
        }
    }

    /// Whether it swaps width and height.
    #[must_use]
    pub const fn swaps_axes(self) -> bool {
        matches!(self, Self::R90 | Self::R270)
    }
}

/// How to turn a buffer upright: mirror horizontally first (when
/// `mirrored`), then rotate clockwise.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Orientation {
    pub mirrored: bool,
    pub rotation: Rotation,
}

impl Orientation {
    /// Already upright.
    pub const UPRIGHT: Self = Self {
        mirrored: false,
        rotation: Rotation::R0,
    };

    #[must_use]
    pub const fn is_upright(self) -> bool {
        !self.mirrored && matches!(self.rotation, Rotation::R0)
    }

    /// The size of a `size` buffer once this orientation is applied.
    #[must_use]
    pub const fn applied_size(self, size: Size) -> Size {
        if self.rotation.swaps_axes() {
            size.transposed()
        } else {
            size
        }
    }

    /// `self` applied first, then `then`.
    #[must_use]
    pub const fn then(self, then: Self) -> Self {
        // Mirror-then-rotate composes like the dihedral group D4: a
        // mirror after a rotation r equals the rotation -r after a mirror.
        let first = self.rotation.quarter_turns();
        let second = then.rotation.quarter_turns();
        let turns = if then.mirrored {
            (4 - first % 4) % 4 + second
        } else {
            first + second
        };
        Self {
            mirrored: self.mirrored != then.mirrored,
            rotation: Rotation::from_quarter_turns(turns),
        }
    }

    /// A vertical flip (`Y_INVERT`) expressed as mirror + rotation.
    pub const Y_INVERT: Self = Self {
        mirrored: true,
        rotation: Rotation::R180,
    };
}

/// Facts about a source that are plain yes / no.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent facts the platforms report one by one"
)]
pub struct SourceFlags {
    pub primary: bool,
    pub minimised: bool,
    pub occluded: bool,
    /// The OS reports its content as protected (masked in captures).
    pub protected: bool,
    /// The source belongs to this process.
    pub own_process: bool,
}

/// How a listed source can be captured.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum SourceCapture {
    /// By id (`SourceSelector::Id`) on the backend that issued it.
    #[default]
    ById,
    /// Listed from the compositor's window list only: no backend here
    /// captures it by id (`Unsatisfiable(WindowById)`). A picker of one
    /// of these backends can (`ConsentConfig::allow_picker`); empty when
    /// no picker backend runs in this session.
    PickerOnly(Vec<BackendId>),
}

/// A capturable monitor, window or application.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Source {
    pub id: SourceId,
    pub kind: SourceKind,
    pub name: String,
    /// Wayland `app_id`, bundle id, AUMID or package.
    pub app_id: Option<String>,
    pub pid: Option<u32>,
    /// Logical geometry in the desktop's global space.
    pub geometry: Option<Rect>,
    /// Physical pixel size.
    pub physical_size: Option<Size>,
    /// 1000 = 1.0.
    pub scale_permille: Option<u32>,
    pub refresh_mhz: Option<u32>,
    /// What the backend's buffers need to be shown upright.
    pub orientation: Orientation,
    pub flags: SourceFlags,
    /// Monitors: the panel's manufacturer as the platform names it
    /// (Wayland `wl_output` make, the EDID PNP id on X11).
    pub make: Option<String>,
    /// Monitors: the panel's model name.
    pub model: Option<String>,
    /// Monitors: the connector or output name (`DP-1`, `HDMI-A-1`).
    pub connector: Option<String>,
    /// Monitors: the platform's human-readable description.
    pub description: Option<String>,
    pub capture: SourceCapture,
}

impl Source {
    /// A source with only the required fields set.
    #[must_use]
    pub fn new(id: SourceId, kind: SourceKind, name: impl Into<String>) -> Self {
        Self {
            id,
            kind,
            name: name.into(),
            app_id: None,
            pid: None,
            geometry: None,
            physical_size: None,
            scale_permille: None,
            refresh_mhz: None,
            orientation: Orientation::UPRIGHT,
            flags: SourceFlags::default(),
            make: None,
            model: None,
            connector: None,
            description: None,
            capture: SourceCapture::ById,
        }
    }

    /// Whether `SourceSelector::Id` can name it.
    #[must_use]
    pub const fn capturable_by_id(&self) -> bool {
        matches!(self.capture, SourceCapture::ById)
    }

    /// Scale, defaulting to 1.0 when the backend does not know it.
    #[must_use]
    pub fn scale(&self) -> u32 {
        self.scale_permille.filter(|s| *s > 0).unwrap_or(1000)
    }
}

/// Which space a region's rectangle is in.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum RegionSpace {
    /// The desktop's global logical space (may span monitors).
    Global,
    /// Logical pixels relative to this monitor's top-left corner.
    MonitorRelative(SourceId),
}

/// A rectangle to capture.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RegionSelector {
    pub rect: Rect,
    pub space: RegionSpace,
}

/// Restrictions for a picker the platform shows.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct PickerOptions {
    /// Kinds offered; empty = whatever the platform offers.
    pub kinds: Vec<SourceKind>,
    /// Let the person choose several monitors (one composite).
    pub multiple: bool,
}

/// What to capture.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum SourceSelector {
    Id(SourceId),
    /// A region; it may span monitors.
    Region(RegionSelector),
    /// Whatever the person chooses in the platform picker
    /// (`ConsentConfig::allow_picker` must be set).
    Picker(PickerOptions),
    #[default]
    PrimaryMonitor,
    /// One composite of every monitor.
    AllMonitors,
    /// One composite of these monitors.
    Monitors(Vec<SourceId>),
    Focused,
    UnderCursor,
}

/// How a backend's sources can be listed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SourceListing {
    Enumerable,
    /// Listed once a standing grant exists (macOS before TCC).
    AfterGrant,
    /// Only a picker can name a source (portal, iOS, Android, web).
    PickerOnly,
}

/// The answer to `sources`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SourceList {
    pub sources: Vec<Source>,
    pub listing: SourceListing,
}

/// Which sources `sources` should return.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct SourceFilter {
    /// Kinds to list; empty = every kind.
    pub kinds: Vec<SourceKind>,
    /// One backend; `None` = the first runnable backend in `Auto` order
    /// that can list.
    pub backend: Option<BackendId>,
}

impl SourceFilter {
    /// Whether `kind` passes the filter.
    #[must_use]
    pub fn admits(&self, kind: SourceKind) -> bool {
        self.kinds.is_empty() || self.kinds.contains(&kind)
    }
}
