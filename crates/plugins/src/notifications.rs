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

use istmo_macros::{message, plugin};

/// Wire identifier of the notifications plugin.
pub const NOTIFICATIONS_PLUGIN_ID: &str = "istmo.notifications";

/// Android notification-channel importance level. Maps to the constants on
/// `NotificationManagerCompat.IMPORTANCE_*`. iOS ignores the value — the OS
/// enforces interruption levels via user settings.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationImportance {
    /// No sound, no visual interruption.
    Min,
    /// Silent, but visible in the shade.
    Low,
    /// Silent unless the channel has been elevated by the user.
    Default,
    /// Heads-up, sound, badge — reserved for time-sensitive events.
    High,
}

/// Payload for [`Notifications::schedule`].
///
/// `delay_seconds` requests a delayed post — 0 or omitted means "now".
/// `tag` is an optional deduplication key: subsequent posts with the same
/// `tag` replace the previous notification instead of stacking. On Android
/// the tag doubles as the second argument to `NotificationManager.notify`;
/// on iOS it maps to `UNNotificationRequest.identifier`.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationRequest {
    pub title: String,
    pub body: String,
    /// Android notification channel id. Ignored on iOS. The channel is
    /// created on first use — the native side calls
    /// `NotificationManagerCompat.createNotificationChannel` under the
    /// covers when a fresh id shows up.
    pub channel_id: String,
    pub importance: NotificationImportance,
    /// Delay in seconds before posting. `None` or `Some(0)` posts
    /// immediately.
    pub delay_seconds: Option<u32>,
    /// Deduplication tag; `None` means the platform allocates a fresh id
    /// for every post.
    pub tag: Option<String>,
}

/// Handle to a scheduled notification, used by
/// [`Notifications::cancel`] to dismiss it.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotificationHandle {
    /// Platform id: Android `notificationId`, iOS `UNNotificationRequest`
    /// identifier hash. Distinct namespace from
    /// [`istmo_core::NativeHandleId`] — this id round-trips through the
    /// wire without RAII cleanup.
    pub id: u32,
}

/// Reasons a notification could not be posted.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotificationError {
    /// The OS blocks notifications from this app.
    PermissionDenied,
    /// The channel id could not be created (Android policy denies it, or
    /// the process lacks the channel-management permission on very old
    /// OEM builds).
    InvalidChannel(String),
    /// Free-form scheduler failure. Reserved for platform errors the
    /// backend cannot classify.
    Scheduler(String),
}

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
