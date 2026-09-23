//! Stylus / pencil input plugin for [`istmo`](https://docs.rs/istmo).
//!
//! Exposes a single [`Pen`] trait that produces two flume streams —
//! [`PenEvent`] for pressed / drag / lift samples and [`PenHoverEvent`]
//! for cursor-preview samples while the tool is proximate but not
//! touching. Each [`PenClient`] instance is scoped to one native window
//! via [`PenConfig::window_id`]; multi-window apps register N clients
//! sharing the same runtime.
//!
//! Native backends live under `native/{android,ios,macos}/` for
//! platforms that need bridge code in a foreign language; the Windows
//! and Linux backends are Rust-native and gated on `cfg(target_os = …)`
//! in the [`publisher`] module (feature-free — cross-compilation picks
//! the right impl automatically).
//!
//! Enable the `codegen` feature to expose the plugin's `Contract`
//! (see [`istmo-build`](https://docs.rs/istmo-build)) without going
//! through the build-script handover.

#![doc(html_root_url = "https://docs.rs/istmo-pen")]

#[cfg(feature = "codegen")]
pub mod codegen;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub mod backend;

#[cfg(any(target_os = "windows", target_os = "linux", target_os = "macos"))]
pub mod publisher;

use istmo_core::CancelToken;
use istmo_macros::{message, plugin, stream};

/// Wire identifier for the pen plugin.
pub const PEN_PLUGIN_ID: &str = "istmo.pen";

/// Instance-scoped configuration for a [`PenClient`].
///
/// The `window_id` is an app-assigned opaque identifier — the native
/// backend maps it to a specific `View` / `UIView` / `NSWindow` / `HWND`
/// / `wl_surface` at registration time. Two [`PenClient`]s created with
/// different ids receive disjoint event streams.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PenConfig {
    /// Opaque per-window identifier. Convention: monotonic small
    /// integers assigned as windows are opened.
    pub window_id: u64,
}

impl PenConfig {
    #[must_use]
    pub const fn new(window_id: u64) -> Self {
        Self { window_id }
    }
}

/// Which tool produced a [`PenSample`].
///
/// Backends distinguish primary tool tip from eraser / secondary tools
/// where the platform reports it; consumers filter by `tool_id` on
/// [`PenSample`] to route strokes appropriately.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PenToolKind {
    /// Primary tip — the default drawing surface of a stylus.
    Tip,
    /// Eraser end of a stylus (or an Apple Pencil configured for
    /// erase).
    Eraser,
    /// A tool whose classification the backend could not resolve.
    Unknown,
}

/// A single input sample from a pen / stylus.
///
/// Every field is present on every sample even when the platform does
/// not provide the underlying signal — unavailable channels are
/// reported as `0.0` and gated by the matching flag in
/// [`PenCapabilities`]. This keeps the wire schema flat and cheap to
/// decode; consumers that care about signal presence check
/// capabilities once at attach time.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct PenSample {
    /// X position in logical (points / dp) coordinates relative to the
    /// registered window's origin.
    pub x: f32,
    /// Y position in logical coordinates relative to the window origin
    /// (grows downward, matching every platform's convention).
    pub y: f32,
    /// Normalised pressure, `0.0..=1.0`. `0.0` when the tool is hovering
    /// (see [`PenHoverEvent`]) or the platform reports no pressure.
    pub pressure: f32,
    /// Tilt around the X axis, radians. Zero when unavailable.
    pub tilt_x: f32,
    /// Tilt around the Y axis, radians. Zero when unavailable.
    pub tilt_y: f32,
    /// Azimuth angle around the Z axis, radians, `-PI..=PI`. Zero when
    /// unavailable.
    pub azimuth: f32,
    /// Altitude above the surface, radians, `0..=PI/2`. Zero when
    /// unavailable.
    pub altitude: f32,
    /// Barrel-roll / twist around the pen's long axis, radians,
    /// `0..=2*PI`. Zero when unavailable.
    pub twist: f32,
    /// Barrel-pressure squeeze (macOS `tangentialPressure`), `-1.0..=1.0`.
    /// Zero when unavailable.
    pub tangential_pressure: f32,
    /// Hover z-offset above the surface, logical units. Zero when in
    /// contact.
    pub z_offset: f32,
    /// Monotonic microseconds elapsed since the native backend attached
    /// to the window. Normalised across platforms so consumer code does
    /// not care whether the native clock is `uptimeMillis` /
    /// `CFAbsoluteTime` / `GetTickCount`.
    pub timestamp_us: u64,
    /// Per-window strictly-increasing sequence number. Predicted /
    /// coalesced samples share the sequence of the parent live sample
    /// so consumers can invalidate predicted state when a real sample
    /// arrives with `sequence >= predicted.last().sequence`.
    pub sequence: u32,
    /// Backend-assigned tool identifier. Stable across the same tool's
    /// lifetime; disambiguates multi-tool arbitration (e.g. Wacom
    /// stylus + eraser, or two Apple Pencils paired to one iPad).
    pub tool_id: u32,
    /// Which end of the tool produced this sample.
    pub tool_kind: PenToolKind,
    /// Bitmap of currently pressed barrel / side buttons — LSB is the
    /// primary button, bit 1 is the secondary, bit 2 is the tertiary
    /// (some Wacom / Linux tablets), and so on. Consumers building
    /// configuration UIs should size the bindings surface by
    /// [`PenCapabilities::button_count`] and mask this field
    /// accordingly. Bits at or above `button_count` are undefined.
    pub buttons: u32,
}

/// Extra payload for [`PenEvent::Move`]. Wrapped in a struct because
/// the current codegen (`istmo-build::kotlin_types`) models
/// single-payload variants only — multi-tuple variants would truncate
/// on the Kotlin / Swift side.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct PenMove {
    /// Live sample this move event reports.
    pub sample: PenSample,
    /// High-rate intermediate samples the platform captured between
    /// the previous [`PenEvent::Move`] and this one (Android
    /// `getHistorical*`, iOS `coalescedTouches`, Windows
    /// `GetPointerPenInfoHistory`). Empty when unsupported.
    pub coalesced: Vec<PenSample>,
    /// Platform-forecast future samples when prediction is enabled
    /// (iOS `predictedTouches`). Empty on every other platform.
    pub predicted: Vec<PenSample>,
}

/// Extra payload for [`PenEvent::ButtonChanged`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct PenButtonChange {
    /// Sample observed at the button transition.
    pub sample: PenSample,
    /// Bitmap of buttons whose state flipped since the previous
    /// sample; the new pressed set lives on `sample.buttons`.
    pub changed: u32,
}

/// A contact-phase event fired while the tool is touching the surface.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub enum PenEvent {
    /// Pen made contact with the surface.
    Down(PenSample),
    /// Pen moved while in contact.
    Move(PenMove),
    /// Pen lifted off the surface.
    Up(PenSample),
    /// Contact was cancelled by the platform (system gesture, palm
    /// rejection, screen locked). Draws in progress should be
    /// discarded.
    Cancel(PenSample),
    /// Barrel button state changed.
    ButtonChanged(PenButtonChange),
}

/// A hover-phase event fired while the tool is proximate but not
/// touching the surface. Only surfaces on platforms with proximity
/// sensing (iPadOS 12.9"+ with Apple Pencil, Android `AXIS_DISTANCE`,
/// Windows `WM_POINTERENTER` / `HOVER` / `LEAVE`).
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub enum PenHoverEvent {
    /// Tool entered proximity of the surface.
    ProximityEnter(PenSample),
    /// Tool moved while hovering.
    Move(PenSample),
    /// Tool left proximity.
    ProximityLeave,
}

/// Which optional signals the current device / OS combo populates.
///
/// Consumers read this once at attach time and gate UI accordingly (a
/// palette that only shows tilt-driven brushes when `tilt` is `true`,
/// etc.). Fields that the sample carries as zero but which the
/// capability flag reports `true` are still meaningful — a tool at rest
/// legitimately reports zero pressure.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PenCapabilities {
    pub pressure: bool,
    pub tilt: bool,
    pub azimuth: bool,
    pub altitude: bool,
    pub twist: bool,
    pub tangential_pressure: bool,
    pub hover: bool,
    pub predicted: bool,
    pub coalesced: bool,
    /// Number of on-tool buttons the platform can report on
    /// [`PenSample::buttons`]. Zero means the tool has no side buttons
    /// (Apple Pencil 1/2/Pro report buttons through
    /// `UIPencilInteraction` rather than per-touch state — they surface
    /// as zero here). Typical stylus values: Wacom pens report 2 or 3
    /// (barrel + eraser as tail); Android reports 2 (`BUTTON_STYLUS_PRIMARY`
    /// + `_SECONDARY`); Windows reports 2 (barrel + second-button via
    /// `POINTER_FLAG_SECONDBUTTON`); macOS reports 2 (barrel + tail
    /// button through `NSEvent.buttonMask`). Consumers use this to
    /// decide how many "action" bindings a UI settings panel should
    /// expose.
    pub button_count: u32,
    pub eraser: bool,
}

/// Domain-level errors returned by [`Pen`] methods.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PenError {
    /// No native window has been registered for the [`PenConfig::window_id`]
    /// this client was created with. Register the window via the
    /// per-platform publisher (desktop) or `PenBackendImpl(context,
    /// view, config)` (mobile) before creating the client.
    NotAttached,
    /// Native backend refused the request. Carries the platform's raw
    /// error message.
    Backend(String),
}

impl std::fmt::Display for PenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotAttached => f.write_str("no window registered for this PenConfig.window_id"),
            Self::Backend(msg) => write!(f, "pen backend error: {msg}"),
        }
    }
}

impl std::error::Error for PenError {}

#[plugin(
    name = "istmo.pen",
    init = PenConfig,
    crate = "::istmo_core",
)]
pub trait Pen {
    /// Contact-phase event stream. Closes when the client drops.
    #[stream]
    fn events(&self) -> PenEvent;

    /// Hover-phase event stream. Closes when the client drops.
    #[stream]
    fn hover(&self) -> PenHoverEvent;

    /// Report which optional signals this device / OS populates.
    async fn capabilities(&self) -> Result<PenCapabilities, PenError>;

    /// Toggle platform-supplied predicted samples on [`PenEvent::Move`]
    /// events. iOS returns non-empty `predicted` when enabled; every
    /// other platform ignores the setting (`predicted` stays empty).
    async fn set_prediction_enabled(
        &self,
        cancel: CancelToken,
        enabled: bool,
    ) -> Result<(), PenError>;
}
