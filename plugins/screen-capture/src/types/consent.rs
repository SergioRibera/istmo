//! Consent, standing grants and restore tokens.

use istmo_core::NativeHandleId;
use istmo_macros::message;

use super::caps::Persistence;
use super::source::BackendId;

/// A standing, system-setting grant.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessScope {
    /// macOS Screen Recording (TCC).
    ScreenRecording,
    /// Windows borderless WGC capture.
    BorderlessCapture,
    /// Android 13+ notifications (the projection's FGS notification).
    Notifications,
    Microphone,
}

/// Ask for a standing grant.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct AccessRequest {
    pub scope: AccessScope,
}

/// The state of a standing grant: shared with the other media plugins.
pub use istmo_media::AccessStatus;

/// How a backend asks for consent.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConsentModel {
    /// Nobody asks (direct Wayland, X11, WGC).
    NoneNeeded,
    SystemSetting,
    /// A dialog per session (portal, Android).
    PerSessionDialog,
    /// A picker per session (iOS 27, web).
    PickerPerSession,
    /// A system alert per session (ReplayKit).
    PerSessionSystemAlert,
}

/// What the OS shows the person while capture runs.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum IndicatorState {
    Shown,
    /// Not shown, and why.
    Hidden(String),
    NotApplicable,
}

/// A standing grant, as `access` / `request_access` report it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AccessState {
    pub scope: AccessScope,
    pub status: AccessStatus,
    pub model: ConsentModel,
    pub persistence: Persistence,
    pub revocable_mid_session: bool,
    pub indicator: IndicatorState,
}

/// Whether consent should outlive the session.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PersistMode {
    #[default]
    DoNot,
    WhileRunning,
    UntilRevoked,
}

/// A token that skips the consent dialog next time. As sensitive as the
/// consent itself: store it in the platform secret store, never in logs
/// or plain files. `Debug` prints `<redacted>` for the value.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RestoreToken {
    #[redact]
    pub value: String,
    pub backend: BackendId,
    pub plugin_version: u32,
}

/// The token format this build writes and accepts. A token from another
/// version or backend is ignored (the dialog shows).
pub const RESTORE_TOKEN_VERSION: u32 = 1;

/// Android: a MediaProjection consent kept for one later `open` /
/// `screenshot`. Only the handle id crosses the bus.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProjectionGrant {
    pub handle: NativeHandleId,
}

/// What a MediaProjection consent should offer.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProjectionMode {
    /// The person chooses (entire screen or one app, Android 14+).
    #[default]
    UserChoice,
    EntireScreen,
    SingleApp,
}

/// `request_projection` arguments.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct ProjectionRequest {
    pub mode: ProjectionMode,
}

/// Per-session consent options.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct ConsentConfig {
    /// `false`: `ConsentRequired` / `Unsatisfiable` instead of any UI.
    pub allow_picker: bool,
    pub restore_token: Option<RestoreToken>,
    pub persist: PersistMode,
    /// Android: from `request_projection`.
    pub grant: Option<ProjectionGrant>,
}
