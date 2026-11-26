//! Wire types of the tray contract.

use std::fmt;

use istmo_core::NativeHandleId;
use istmo_macros::message;
use istmo_menu::{CheckState, IconRef, ItemId, MenuError, MenuNode, RadioGroupId};

/// Phantom tag of a tray item handle.
#[derive(Debug, Clone, Copy)]
pub struct TrayTag(std::convert::Infallible);

/// Phantom tag of a registered icon handle.
#[derive(Debug, Clone, Copy)]
pub struct IconTag(std::convert::Infallible);

/// The answer to `create`. `create_owned` wraps `tray` in a handle that
/// removes the item on drop.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrayItem {
    #[handle(TrayTag)]
    pub tray: NativeHandleId,
}

/// The answer to `register_icon`. `register_icon_owned` wraps `icon` in
/// a handle that frees the decoded icon on drop.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RegisteredIcon {
    #[handle(IconTag)]
    pub icon: NativeHandleId,
}

/// The icon a tray item shows.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrayIcon {
    Static(IconRef),
    Animated(Animation),
}

impl From<IconRef> for TrayIcon {
    fn from(icon: IconRef) -> Self {
        Self::Static(icon)
    }
}

impl From<istmo_menu::IconImage> for TrayIcon {
    fn from(image: istmo_menu::IconImage) -> Self {
        Self::Static(IconRef::Inline(image))
    }
}

/// Frames the plugin cycles through on its own timer.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Animation {
    pub frames: Vec<AnimationFrame>,
    pub repeat: Repeat,
}

/// One frame of an [`Animation`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AnimationFrame {
    pub icon: IconRef,
    /// Raised to the backend's `min_frame_ms`.
    pub duration_ms: u32,
}

/// How often an [`Animation`] runs.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Repeat {
    Forever,
    /// Runs this many times, then keeps showing the last frame.
    Times(u32),
}

/// What a left click on the icon does.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PrimaryAction {
    /// Opens the menu.
    #[default]
    ShowMenu,
    /// Emits [`TrayEvent::Activate`]; a right click opens the menu.
    Activate,
}

/// Text shown when hovering the icon.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Tooltip {
    pub title: String,
    pub body: Option<String>,
}

impl From<&str> for Tooltip {
    fn from(title: &str) -> Self {
        Self {
            title: title.to_owned(),
            body: None,
        }
    }
}

/// Everything a tray item starts with.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TraySpec {
    /// Stable per app: the `StatusNotifierItem` `Id`, the macOS autosave
    /// name that keeps the user's placement across runs.
    pub id: String,
    pub icon: TrayIcon,
    pub title: Option<String>,
    pub tooltip: Option<Tooltip>,
    pub menu: Vec<MenuNode>,
    pub primary: PrimaryAction,
    /// Windows: identify the icon by this GUID
    /// (`xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx`) so Windows remembers the
    /// user's visibility choice across runs. Binds the registration to
    /// the executable's path: a moved executable cannot add the icon.
    pub windows_guid: Option<String>,
}

impl TraySpec {
    /// A visible item with an icon, no menu, no title or tooltip.
    pub fn new(id: impl Into<String>, icon: impl Into<TrayIcon>) -> Self {
        Self {
            id: id.into(),
            icon: icon.into(),
            title: None,
            tooltip: None,
            menu: Vec::new(),
            primary: PrimaryAction::default(),
            windows_guid: None,
        }
    }

    #[must_use]
    pub fn with_menu(mut self, menu: Vec<MenuNode>) -> Self {
        self.menu = menu;
        self
    }

    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    #[must_use]
    pub fn with_tooltip(mut self, tooltip: impl Into<Tooltip>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    #[must_use]
    pub const fn with_primary(mut self, primary: PrimaryAction) -> Self {
        self.primary = primary;
        self
    }
}

/// A rectangle in screen coordinates, physical pixels.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// Where the icon is on screen, as far as the platform tells.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Anchor {
    pub rect: Rect,
    /// Scale of the monitor holding the rect (1.0 when unknown).
    pub scale: f64,
    pub precision: AnchorPrecision,
}

/// How much an [`Anchor`] can be trusted.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum AnchorPrecision {
    /// The icon's own rectangle.
    IconRect,
    /// The point clicked; the rect is 0 × 0.
    ClickPoint,
    /// A point from the tray host whose meaning varies by host
    /// (Wayland panels often send 0, 0).
    Unreliable,
}

/// A click on the icon.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Pointer {
    pub anchor: Option<Anchor>,
}

/// A scroll over the icon.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ScrollDelta {
    /// Steps as the host reports them; positive is down / right.
    pub delta: i32,
    pub orientation: Orientation,
}

/// Axis of a [`ScrollDelta`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Orientation {
    Vertical,
    Horizontal,
}

/// A check item's new state after it was activated.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct CheckChange {
    pub item: ItemId,
    pub state: CheckState,
}

/// A radio item became the selected one of its group.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct RadioChange {
    pub group: RadioGroupId,
    pub item: ItemId,
}

/// Whether something shows tray items right now.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum HostState {
    Available,
    /// Nothing shows tray items (GNOME without the `AppIndicator`
    /// extension). Items appear when a host does.
    Missing,
    /// The tray host restarted (explorer.exe, the panel); items were
    /// added again.
    Restarted,
}

/// Something that happened to a tray item.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TrayEvent {
    /// Left click with [`PrimaryAction::Activate`].
    Activate(Pointer),
    /// Middle click.
    SecondaryActivate(Pointer),
    DoubleClick(Pointer),
    Scroll(ScrollDelta),
    MenuOpened,
    MenuClosed,
    /// An item was activated (checks and radios after their automatic
    /// state change).
    ItemActivated(ItemId),
    CheckChanged(CheckChange),
    RadioSelected(RadioChange),
    Host(HostState),
}

/// The platform service a backend talks to.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum TrayBackend {
    /// Linux `StatusNotifierItem` + `com.canonical.dbusmenu`.
    Sni,
    /// Windows notification area.
    Win32,
    /// macOS menu bar.
    AppKit,
    /// The browser tab's favicon.
    Favicon,
    /// No tray on this platform.
    Unavailable,
}

/// How a backend provides a feature.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Support {
    Native,
    /// Built by the plugin from other platform features.
    Emulated,
    /// Sent to the tray host, which may or may not show it.
    HostDependent,
    /// Ignored.
    Unsupported,
}

/// Menu features per backend.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MenuSupport {
    pub check: Support,
    pub mixed: Support,
    pub radio: Support,
    pub item_icons: Support,
    pub shortcuts: Support,
    pub header: Support,
    pub subtitle: Support,
    pub badge: Support,
    pub default_item: Support,
    pub mnemonics: Support,
    /// Changes show while the menu is open.
    pub live_while_open: Support,
}

/// Events per backend.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EventSupport {
    pub activate: Support,
    pub secondary: Support,
    pub double_click: Support,
    pub scroll: Support,
    pub menu_open_close: Support,
}

/// What the running backend does natively, emulates or ignores.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrayCapabilities {
    pub backend: TrayBackend,
    pub host: HostState,
    /// Most items at once; `None` = no limit.
    pub max_items: Option<u32>,
    /// Shortest animation frame the backend shows.
    pub min_frame_ms: u32,
    pub image_names: Support,
    pub template_images: Support,
    pub title: Support,
    pub tooltip_body: Support,
    pub menu: MenuSupport,
    pub events: EventSupport,
    pub anchor: Option<AnchorPrecision>,
}

/// A call or part of a spec the backend cannot honour at all.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Feature {
    Tray,
    Menu,
    Title,
    Tooltip,
    Visibility,
    PlatformNames,
}

/// A bound a call went past.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LimitKind {
    /// More tray items than the backend shows.
    Items,
    /// An image file larger than the plugin decodes.
    ImageBytes,
    /// A decoded image larger than the plugin keeps.
    ImageSize,
    /// More animation frames than the plugin keeps.
    Frames,
    /// A text longer than the plugin accepts.
    Text,
}

/// Why an image was refused.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ImageError {
    /// The animation frame, `0` for a static icon.
    pub frame: u32,
    /// The representation within the icon.
    pub rep: u32,
    pub reason: ImageFailure,
}

/// What was wrong with an image.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImageFailure {
    /// The icon lists no representation the backend can show.
    NothingUsable,
    /// The bytes do not decode.
    Decode(String),
    /// The format's cargo feature is off.
    FormatNotCompiled,
    /// The image has no pixels.
    Empty,
}

/// Why a tray call failed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum TrayError {
    Unsupported(Feature),
    UnknownTray,
    UnknownIcon,
    InvalidMenu(MenuError),
    Image(ImageError),
    Limit(LimitKind),
    /// An OS call failed; the text is for logs.
    Platform(String),
    ShutDown,
    /// The runtime answered with an error of its own; the text is for
    /// logs.
    Bus(String),
}

impl fmt::Display for TrayError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unsupported(feature) => write!(f, "tray: {feature:?} is not supported here"),
            Self::UnknownTray => f.write_str("tray: no such tray item"),
            Self::UnknownIcon => f.write_str("tray: no such registered icon"),
            Self::InvalidMenu(err) => write!(f, "tray: invalid menu: {err}"),
            Self::Image(err) => write!(
                f,
                "tray: icon frame {} representation {}: {}",
                err.frame, err.rep, err.reason
            ),
            Self::Limit(kind) => write!(f, "tray: {kind:?} limit exceeded"),
            Self::Platform(msg) => write!(f, "tray: platform error: {msg}"),
            Self::ShutDown => f.write_str("tray: the backend has shut down"),
            Self::Bus(msg) => write!(f, "tray: {msg}"),
        }
    }
}

impl fmt::Display for ImageFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NothingUsable => f.write_str("no representation this backend can show"),
            Self::Decode(msg) => write!(f, "does not decode: {msg}"),
            Self::FormatNotCompiled => f.write_str("format not compiled in"),
            Self::Empty => f.write_str("image has no pixels"),
        }
    }
}

impl std::error::Error for TrayError {}

impl From<MenuError> for TrayError {
    fn from(err: MenuError) -> Self {
        Self::InvalidMenu(err)
    }
}

impl From<istmo_core::IstmoError> for TrayError {
    fn from(err: istmo_core::IstmoError) -> Self {
        match err {
            istmo_core::IstmoError::PluginError { bytes } => {
                istmo_core::codec::decode::<Self>(&bytes).map_or_else(
                    |e| Self::Bus(format!("decode plugin error payload: {e}")),
                    |(err, _)| err,
                )
            }
            other => Self::Bus(other.to_string()),
        }
    }
}
