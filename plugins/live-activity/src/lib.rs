//! Live Activities plugin.
//!
//! Cross-platform ongoing-status surface: dynamic-island / lock-screen
//! updates on iOS (ActivityKit) and tiered notification renderers on
//! Android (custom `RemoteViews`, `Notification.ProgressStyle`, Live
//! Updates status-bar chip).
//!
//! The plugin trait is intentionally non-generic — Rust callers pick up
//! type safety through [`TypedLiveActivity`], a lightweight generic
//! wrapper that bincode-encodes attributes / state on the way out and
//! ships them across the wire as opaque byte payloads tagged by
//! `activity_type`. The native side dispatches each type to a registered
//! backend (`ActivityAttributes` conformer on iOS, `LiveActivityBackend`
//! implementor on Android) that knows how to decode its own payload
//! shape.
//!
//! Wire identifier is `istmo.live_activity` under the `istmo.` core
//! namespace.
//!
//! # Usage sketch
//!
//! ```text
//! use istmo_live_activity::{ActivityStyle, TypedLiveActivity};
//!
//! let activities = TypedLiveActivity::<TimerAttributes, TimerState>::new(rt, "timer")?;
//! let handle = activities
//!     .start(
//!         TimerAttributes { title: "Focus block".into() },
//!         TimerState { elapsed: 0 },
//!         ActivityStyle::Standard,
//!     )
//!     .await?;
//! activities.update(handle.id(), TimerState { elapsed: 30 }, None).await?;
//! ```
//!
//! `TimerAttributes` / `TimerState` are author-defined `#[istmo::message]`
//! types; see `plugins/live-activity/tests/live_activity.rs` for a runnable
//! shape.

#[cfg(feature = "codegen")]
pub mod codegen;

mod typed;

use istmo_macros::{message, plugin};

pub use typed::TypedLiveActivity;

/// Wire identifier of the live-activity plugin.
pub const LIVE_ACTIVITY_PLUGIN_ID: &str = "istmo.live_activity";

/// Marker type used as the phantom parameter of the activity handle. Only
/// its identity matters — the value is never constructed.
///
/// A `NativeHandle<LiveActivityToken>` refers to the platform-owned live
/// activity object: an `ActivityKit.Activity<Attributes>` on iOS, an
/// ongoing-notification id on Android. Drop fires
/// `Frame::ReleaseNativeHandle`; the native side interprets it as
/// "end this activity immediately".
#[non_exhaustive]
#[derive(Debug)]
pub enum LiveActivityToken {}

// ---------------------------------------------------------------------------
// Wire types
// ---------------------------------------------------------------------------

/// Presentation style requested at start time.
///
/// `Standard` is the classic ActivityKit shape — persistent until ended.
/// `Transient` maps to iOS 18's `.transient` style (auto-dismisses after
/// a short display window). Android's tiered renderers currently treat
/// both the same — the flag is preserved on the wire so future Android
/// versions can honour it.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityStyle {
    Standard,
    Transient,
}

/// Optional alert configuration attached to an update.
///
/// Emits a heads-up display + sound / haptic on iOS (via
/// `AlertConfiguration`) and raises the notification priority to
/// `HIGH` on Android for the duration of the update.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlertConfig {
    pub title: String,
    pub body: String,
    pub sound: AlertSound,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertSound {
    /// System default alert sound (`.default` on iOS, `DEFAULT_SOUND` on Android).
    Default,
    /// Bundled resource by name — Swift `Bundle.main` / Android `res/raw/<name>`.
    Named(String),
    /// Silent update — visible refresh only, no audible alert.
    None,
}

/// Dismissal behaviour when [`LiveActivity::end`] is called.
///
/// * `Immediate` — remove the activity from the lock screen / notification
///   panel right away.
/// * `Default` — keep it visible for the platform default (iOS: 4h grace
///   window; Android: cleared immediately since no analogue exists).
/// * `AfterSeconds(u32)` — schedule dismissal at the given delay.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DismissalPolicy {
    Immediate,
    Default,
    AfterSeconds(u32),
}

/// Optional Android render-tier hint. `None` lets the backend pick the
/// highest tier the device supports (`Adaptive` strategy).
///
/// See [`AndroidCapabilities`] for the runtime capability probe.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AndroidTierHint {
    /// Force the `RemoteViews` custom layout path regardless of device
    /// capabilities. Useful when the author wants a uniform look across
    /// every Android version.
    ForceCustom,
    /// Prefer system templates (`Notification.ProgressStyle` on API 35+,
    /// promoted `LiveUpdate` chip on API 36+) when available, fall back
    /// to custom on older devices.
    PreferSystemTemplates,
    /// Require the API 36+ Live Updates chip. Surfaces
    /// [`ActivityError::NotSupported`] on any older device.
    RequireLiveUpdate,
}

/// Snapshot of what the current platform can render.
///
/// Query via [`LiveActivity::capabilities`] before calling [`LiveActivity::start`]
/// to build a UX that adapts to the device (e.g. "Live tracking available
/// on your device" copy vs "notification-only fallback").
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlatformCapabilities {
    Ios(IosCapabilities),
    Android(AndroidCapabilities),
    /// Platform has no live-activity surface at all (desktop, WASM, …).
    Unsupported,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IosCapabilities {
    /// True when the OS version is iOS 16.1 or newer.
    pub activity_kit_available: bool,
    /// True when the user has Live Activities enabled in system settings.
    pub activities_enabled: bool,
    /// True when the current device supports Dynamic Island (iPhone 14 Pro+).
    pub dynamic_island: bool,
    /// True when this activity type opted in to push-token updates. Push
    /// support is a future milestone; today this is always `false`.
    pub push_updates: bool,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AndroidCapabilities {
    /// Ongoing notification with custom `RemoteViews`. Available on every
    /// supported Android version (API 21+).
    pub supports_custom: bool,
    /// `Notification.ProgressStyle` system template. API 35 (Android 15) +.
    pub supports_progress_style: bool,
    /// `setPromotedOngoing(true)` + status-bar chip anchor ("Live Updates").
    /// API 36 (Android 16) +.
    pub supports_live_update: bool,
    /// User has not blocked the notification channel used by this plugin.
    pub notifications_enabled: bool,
}

/// A previously-started activity that survived a process restart.
///
/// Returned by [`LiveActivity::restore_active`] so the app can reattach
/// its `NativeHandle` to activities that outlived the previous process.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoredActivity {
    pub handle: istmo_core::NativeHandleId,
    pub activity_type: String,
    /// bincode-encoded attributes (the caller decodes into their concrete `A`).
    pub attributes: Vec<u8>,
    /// bincode-encoded latest state (the caller decodes into their concrete `C`).
    pub state: Vec<u8>,
}

/// Error returned by the plugin surface.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActivityError {
    /// Platform / OS version cannot render the requested tier.
    NotSupported,
    /// User disabled Live Activities globally, or the notification channel
    /// this plugin uses is muted.
    Disabled,
    /// The device is already displaying the platform maximum number of
    /// concurrent activities (iOS caps this).
    ExceededMaximum,
    /// The supplied handle id has no live counterpart on the native side —
    /// either already ended or never issued.
    HandleNotFound,
    /// The `activity_type` string has no registered backend on the native
    /// side. Register one via `IstmoRuntime.registerLiveActivityBackend`
    /// (Kotlin) / `IstmoRuntime.shared.register(activityBackend:for:)`
    /// (Swift) before calling `start`.
    UnknownActivityType(String),
    /// Encoded attributes or state failed to decode on the native side.
    Decode(String),
    /// Any other backend failure surfaced verbatim from the platform.
    Backend(String),
}

impl std::fmt::Display for ActivityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotSupported => f.write_str("live activities are not supported on this device"),
            Self::Disabled => f.write_str("live activities are disabled by the user"),
            Self::ExceededMaximum => f.write_str("exceeded the platform maximum concurrent live activities"),
            Self::HandleNotFound => f.write_str("live-activity handle not found"),
            Self::UnknownActivityType(ty) => {
                write!(f, "no backend registered for activity type {ty:?}")
            }
            Self::Decode(msg) => write!(f, "live-activity payload decode failed: {msg}"),
            Self::Backend(msg) => write!(f, "live-activity backend error: {msg}"),
        }
    }
}

impl std::error::Error for ActivityError {}

// ---------------------------------------------------------------------------
// Plugin surface (wire-level, non-generic).
// ---------------------------------------------------------------------------

/// Live-activity plugin surface.
///
/// Payloads are opaque byte buffers on the wire — the strongly-typed
/// Rust surface lives on top in [`TypedLiveActivity`]. Multiple activity
/// types share the same plugin instance; each type is dispatched to its
/// own native-side backend by the `activity_type` string.
#[plugin(name = "istmo.live_activity", crate = "::istmo_core")]
pub trait LiveActivity {
    /// Start a new live activity of `activity_type`.
    ///
    /// * `attributes` — bincode-encoded immutable attributes attached for
    ///   the lifetime of the activity.
    /// * `initial_state` — bincode-encoded first snapshot of the mutable
    ///   `ContentState`.
    ///
    /// Returns a fresh [`NativeHandleId`](istmo_core::NativeHandleId) the
    /// caller adopts into a [`NativeHandle`](istmo_core::NativeHandle) via
    /// [`TypedLiveActivity`].
    async fn start(
        &self,
        activity_type: String,
        attributes: Vec<u8>,
        initial_state: Vec<u8>,
        style: ActivityStyle,
        stale_after_seconds: Option<u32>,
        android_tier_hint: Option<AndroidTierHint>,
    ) -> Result<istmo_core::NativeHandleId, ActivityError>;

    /// Push a new state snapshot to a running activity.
    ///
    /// `alert` opts the update into a heads-up presentation (lock-screen
    /// wake / haptic on iOS, `HIGH` priority + sound on Android). Pass
    /// `None` for a silent refresh.
    async fn update(
        &self,
        handle: istmo_core::NativeHandleId,
        state: Vec<u8>,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError>;

    /// End a running activity. `final_state` optionally supplies a last
    /// snapshot to display during the dismissal grace window (iOS only —
    /// Android dismisses immediately regardless).
    async fn end(
        &self,
        handle: istmo_core::NativeHandleId,
        final_state: Option<Vec<u8>>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError>;

    /// Convenience probe: `true` when the platform is currently willing
    /// to accept new live activities (iOS setting enabled and channel
    /// unblocked on Android).
    async fn are_activities_enabled(&self) -> Result<bool, ActivityError>;

    /// Full capability snapshot for the current device / OS version.
    async fn capabilities(&self) -> Result<PlatformCapabilities, ActivityError>;

    /// Reattach to activities that survived a process restart. Ordered
    /// arbitrarily by the backend.
    async fn restore_active(&self) -> Result<Vec<RestoredActivity>, ActivityError>;
}
