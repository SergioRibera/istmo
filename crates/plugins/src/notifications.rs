//! Local notifications plugin.
//!
//! Thin wrapper around Android's `NotificationManagerCompat` /
//! `NotificationChannel` API and iOS's `UNUserNotificationCenter`. Scope is
//! intentionally narrow: post a local notification now or after a delay,
//! cancel it, and query / request the OS permission gate.
//!
//! Push notifications (FCM / APNs — a Rust-visible handler wakeable without
//! UI) are a separate future plugin; this one covers the "notify the user
//! from the app process" side only.
//!
//! On Android 13+ the runtime permission `POST_NOTIFICATIONS` gates all
//! notification posts. Callers can go through this plugin's
//! [`Notifications::request_authorization`] as a convenience or wire the
//! permission themselves via the [`crate::permissions`] plugin — both paths
//! trigger the same OS prompt.

use istmo_macros::plugin;

/// Wire identifier of the notifications plugin.
pub const NOTIFICATIONS_PLUGIN_ID: &str = "istmo.notifications";

// `NotificationImportance`, `NotificationRequest`, `NotificationHandle`
// and `NotificationError` are generated from the canonical `Contract`
// builder in `istmo-plugins-schema` via `build.rs`.
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

/// Local notifications plugin surface.
#[plugin(name = "istmo.notifications", crate = "::istmo_core")]
pub trait Notifications {
    /// Returns `true` when the app currently holds notification
    /// authorization. On Android <13 this is always `true` (no runtime
    /// gate). On iOS this queries `UNUserNotificationCenter.getNotificationSettings`.
    async fn is_authorized(&self) -> bool;

    /// Prompts the user for notification authorization. Returns the
    /// terminal decision. Second and subsequent calls short-circuit to the
    /// existing decision — the platform will not re-prompt after an
    /// initial denial.
    async fn request_authorization(&self) -> bool;

    /// Schedule a notification. Immediate posts return once the platform
    /// has accepted the notification (Android `NotificationManagerCompat`
    /// call, iOS `add` completion); delayed posts return once the
    /// scheduler has accepted the request.
    async fn schedule(
        &self,
        request: NotificationRequest,
    ) -> Result<NotificationHandle, NotificationError>;

    /// Cancel a previously scheduled or posted notification. No-op if the
    /// id is unknown (already dismissed by the user, tag replaced, ...).
    async fn cancel(&self, id: u32) -> Result<(), NotificationError>;
}
