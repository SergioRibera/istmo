//! Apple Live Activities / ActivityKit plugin for
//! [`istmo`](https://docs.rs/istmo).
//!
//! Exposes a byte-oriented [`LiveActivity`] trait plus a typed adapter
//! ([`TypedLiveActivity`]) that pins the attributes and content-state
//! types at compile time. Live activity tokens are carried as opaque
//! [`NativeHandle`](istmo_core::NativeHandle)s.
//!
//! On non-Apple platforms the plugin is a no-op stub — callers can
//! share the same Rust code path across platforms without conditional
//! compilation.

#![doc(html_root_url = "https://docs.rs/istmo-live-activity")]

#[cfg(feature = "codegen")]
pub mod codegen;

mod typed;

use istmo_macros::{message, plugin};

pub use typed::TypedLiveActivity;

/// Wire identifier for the live-activity plugin.
pub const LIVE_ACTIVITY_PLUGIN_ID: &str = "istmo.live_activity";

#[non_exhaustive]
#[derive(Debug)]
pub enum LiveActivityToken {}

/// How prominently the platform should render the live activity.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityStyle {
    /// Full presentation — Lock Screen banner + Dynamic Island on iOS,
    /// full ongoing notification with custom RemoteViews on Android.
    Standard,
    /// Compact / minimal presentation — Dynamic Island only on iOS,
    /// slim collapsed notification on Android.
    Transient,
}

/// Optional alert to fire alongside an activity update.
///
/// On iOS the platform decides whether to actually surface the alert
/// (respects Focus modes, do-not-disturb, etc.); on Android it becomes
/// a heads-up notification tied to the ongoing activity.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AlertConfig {
    /// Short title shown as the alert headline.
    pub title: String,
    /// Body copy displayed under the title.
    pub body: String,
    /// Which sound to play with the alert.
    pub sound: AlertSound,
}

/// Sound played when an [`AlertConfig`] fires.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum AlertSound {
    /// Use the platform's default notification sound.
    Default,
    /// Play a named sound bundled with the app — the string maps to a
    /// resource id on Android and a filename in the main bundle on
    /// iOS.
    Named(String),
    /// Silent alert (visual only).
    None,
}

/// How to remove the activity when `end()` is called.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DismissalPolicy {
    /// Remove the activity immediately, no lingering final state.
    Immediate,
    /// Platform default — iOS keeps the activity visible for a few
    /// minutes, Android leaves the ongoing notification until the
    /// system prunes it.
    Default,
    /// Keep the final state visible for the given number of seconds,
    /// then remove.
    AfterSeconds(u32),
}

/// Android-only hint that biases the backend towards a particular
/// rendering strategy when several are available on the device (custom
/// RemoteViews vs. `ProgressStyle` vs. Live Update APIs).
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AndroidTierHint {
    /// Force the fully custom RemoteView-based rendering.
    ForceCustom,
    /// Prefer platform-provided templates when available; fall back to
    /// custom.
    PreferSystemTemplates,
    /// Require the modern Live Update API. Fails on devices that lack
    /// it.
    RequireLiveUpdate,
}

/// Capabilities of the current device for live activities.
///
/// Returned by `capabilities()` so apps can gate features (e.g. only
/// enable Dynamic Island widgets when actually supported).
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlatformCapabilities {
    /// Running on iOS with the ActivityKit surface.
    Ios(IosCapabilities),
    /// Running on Android — capabilities depend on OS version.
    Android(AndroidCapabilities),
    /// Live activities are not available on this device / OS.
    Unsupported,
}

/// iOS-specific ActivityKit feature availability.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IosCapabilities {
    /// Whether the ActivityKit framework itself is available (iOS
    /// 16.1+).
    pub activity_kit_available: bool,
    /// Whether the user has activities enabled in system settings.
    pub activities_enabled: bool,
    /// Whether the device physically has a Dynamic Island.
    pub dynamic_island: bool,
    /// Whether the app is configured to receive APNs push-token
    /// updates for its activities.
    pub push_updates: bool,
}

/// Android-specific live-update / ongoing-notification capabilities.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AndroidCapabilities {
    /// Whether the plugin can render fully custom RemoteViews.
    pub supports_custom: bool,
    /// Whether the platform's `Notification.ProgressStyle` is
    /// available.
    pub supports_progress_style: bool,
    /// Whether the modern Live Update notification API is available
    /// (Android 15+).
    pub supports_live_update: bool,
    /// Whether the user has notifications enabled for this app.
    pub notifications_enabled: bool,
}

/// An activity that survived a process restart and was recovered by
/// the platform.
///
/// Payloads travel as raw bytes here — [`TypedLiveActivity::restore`]
/// decodes them into concrete attributes and state types.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoredActivity {
    /// Handle owning the underlying platform activity — release it
    /// (via `NativeHandle::drop` or `end()`) when done.
    pub handle: istmo_core::NativeHandleId,
    /// Activity-type discriminator matching the `activity_type`
    /// supplied at `start()`.
    pub activity_type: String,
    /// Bincode-encoded attributes as originally passed to `start()`.
    pub attributes: Vec<u8>,
    /// Bincode-encoded latest state observed for the activity.
    pub state: Vec<u8>,
}

/// Domain-level errors returned by every [`LiveActivity`] method.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActivityError {
    /// The device / OS does not offer live activities at all.
    NotSupported,
    /// Live activities are supported but disabled by the user in
    /// system settings.
    Disabled,
    /// Platform-imposed cap on concurrent activities reached — end one
    /// before starting another.
    ExceededMaximum,
    /// Referenced handle no longer maps to a live activity — usually
    /// because the platform ended it out from under us.
    HandleNotFound,
    /// No backend registered for the requested `activity_type`. Add
    /// the handler in the native `LiveActivityBackendImpl`.
    UnknownActivityType(String),
    /// Bincode failed to encode / decode an attributes or state
    /// payload.
    Decode(String),
    /// Any other backend failure. Message is the raw platform error.
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

#[plugin(name = "istmo.live_activity", crate = "::istmo_core")]
pub trait LiveActivity {

    async fn start(
        &self,
        activity_type: String,
        attributes: Vec<u8>,
        initial_state: Vec<u8>,
        style: ActivityStyle,
        stale_after_seconds: Option<u32>,
        android_tier_hint: Option<AndroidTierHint>,
    ) -> Result<istmo_core::NativeHandleId, ActivityError>;

    async fn update(
        &self,
        handle: istmo_core::NativeHandleId,
        state: Vec<u8>,
        alert: Option<AlertConfig>,
    ) -> Result<(), ActivityError>;

    async fn end(
        &self,
        handle: istmo_core::NativeHandleId,
        final_state: Option<Vec<u8>>,
        dismissal: DismissalPolicy,
    ) -> Result<(), ActivityError>;

    async fn are_activities_enabled(&self) -> Result<bool, ActivityError>;

    async fn capabilities(&self) -> Result<PlatformCapabilities, ActivityError>;

    async fn restore_active(&self) -> Result<Vec<RestoredActivity>, ActivityError>;
}

