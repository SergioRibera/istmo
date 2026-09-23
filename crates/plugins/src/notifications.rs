//! Local user notifications.
//!
//! Post, cancel and inspect user-facing notifications. Delivery is
//! best-effort and subject to platform-level channels / importance
//! settings.

use istmo_macros::plugin;

/// Wire identifier for the notifications plugin.
pub const NOTIFICATIONS_PLUGIN_ID: &str = "istmo.notifications";

include!(concat!(env!("OUT_DIR"), "/notifications_types.rs"));

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied => f.write_str("notification permission denied"),
            Self::InvalidChannel(msg) => write!(f, "invalid notification channel: {msg}"),
            Self::Scheduler(msg) => write!(f, "notification scheduler error: {msg}"),
        }
    }
}

impl std::error::Error for NotificationError {}

#[plugin(name = "istmo.notifications", crate = "::istmo_core")]
pub trait Notifications {
    async fn is_authorized(&self) -> bool;

    async fn request_authorization(&self) -> bool;

    async fn schedule(
        &self,
        request: NotificationRequest,
    ) -> Result<NotificationHandle, NotificationError>;

    async fn cancel(&self, id: u32) -> Result<(), NotificationError>;
}
