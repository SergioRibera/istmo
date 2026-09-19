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

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ActivityStyle {
    Standard,
    Transient,
}

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

    Default,

    Named(String),

    None,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DismissalPolicy {
    Immediate,
    Default,
    AfterSeconds(u32),
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AndroidTierHint {

    ForceCustom,

    PreferSystemTemplates,

    RequireLiveUpdate,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum PlatformCapabilities {
    Ios(IosCapabilities),
    Android(AndroidCapabilities),

    Unsupported,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct IosCapabilities {

    pub activity_kit_available: bool,

    pub activities_enabled: bool,

    pub dynamic_island: bool,

    pub push_updates: bool,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AndroidCapabilities {

    pub supports_custom: bool,

    pub supports_progress_style: bool,

    pub supports_live_update: bool,

    pub notifications_enabled: bool,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RestoredActivity {
    pub handle: istmo_core::NativeHandleId,
    pub activity_type: String,

    pub attributes: Vec<u8>,

    pub state: Vec<u8>,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ActivityError {

    NotSupported,

    Disabled,

    ExceededMaximum,

    HandleNotFound,

    UnknownActivityType(String),

    Decode(String),

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

