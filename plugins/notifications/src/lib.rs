//! Local notifications for [`istmo`](https://docs.rs/istmo) on every
//! target, with live updates.
//!
//! One model everywhere: when a platform has a feature the plugin uses
//! it; when it does not, the plugin simulates it and says so in
//! [`Capabilities`]; it refuses only when there is no honest way to do
//! it ([`NotificationError::Unsupported`]).
//!
//! * Show, update in place, cancel ([`Notifications::show`],
//!   [`Notifications::update`], [`Notifications::cancel`]); a
//!   [`LiveNotification`] streams progress without awaiting each step.
//! * Actions, inline replies, choices and snoozes ([`Action`]); what the
//!   user does arrives on [`NotificationResponses`], also at cold start.
//! * Conversations and calls ([`Style`]), pictures and avatars
//!   ([`NotificationImage`]).
//! * Scheduling once, on an interval or on a calendar rule ([`Trigger`],
//!   [`CalendarRule`]), with a policy for missed deliveries.
//!
//! # Hosts
//!
//! * Android / iOS: Kotlin / Swift backends (`native/`), registered by
//!   the app.
//! * Desktop (Linux, Windows, macOS): [`DesktopNotifications`], a Rust
//!   host over a platform [`Backend`](desktop::Backend).
//! * Anything else: [`UnavailableNotifications`].
//! * Tests: `FakeNotifications` (feature `testing`).
//!
//! # Example
//!
//! ```no_run
//! # async fn demo() -> Result<(), istmo_notifications::NotificationError> {
//! use istmo_notifications::{Action, Notification, NotificationsClient, Progress};
//!
//! let client = NotificationsClient::acquire()?;
//! let n = Notification::new("dl:42", "Downloading report.pdf")?
//!     .with_body("3.1 of 8 MB")
//!     .with_progress(Progress::fraction(0.38))
//!     .with_action(Action::button("cancel", "Cancel")?.destructive())
//!     .ongoing();
//! let live = client.show_live(n).await?;
//! live.progress(0.4);
//! live.finish(Notification::new("dl:42", "Download complete")?).await?;
//! # Ok(()) }
//! ```

#![doc(html_root_url = "https://docs.rs/istmo-notifications")]

use istmo_core::HostCtx;
use istmo_macros::{plugin, stream};

mod builder;
mod client;
pub mod coalesce;
#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
pub mod desktop;
#[cfg(feature = "testing")]
mod fake;
mod ids;
mod live;
pub mod render;
mod responses;
pub mod schedule;
mod types;
mod unavailable;
pub mod validate;
#[cfg(target_arch = "wasm32")]
mod web;

#[cfg(not(any(target_os = "android", target_os = "ios", target_arch = "wasm32")))]
pub use desktop::DesktopNotifications;
#[cfg(feature = "testing")]
pub use fake::{FakeCall, FakeNotifications};
pub use ids::{ActionId, ChannelId, GroupId, MAX_ID_LEN, NotificationId, RESERVED_ACTION_PREFIX};
pub use live::LiveNotification;
pub use responses::{
    NotificationResponseStream, NotificationResponses, RESPONSES_CAPACITY, RESPONSES_CHANNEL,
    Response, response_source,
};
pub use types::*;
pub use unavailable::UnavailableNotifications;

/// Wire identifier of the plugin.
pub const NOTIFICATIONS_PLUGIN_ID: &str = "istmo.notifications";

/// The notifications contract.
///
/// `remote = false`: responses are delivered in the main process (the
/// trampoline, delegate or activator runs there), so a `:remote` process
/// has nothing to receive them with.
///
/// Rust clients validate before calling ([`NotificationsClient::show_checked`]
/// and friends); the Rust desktop host validates again, and native
/// backends re-check platform limits.
#[plugin(name = "istmo.notifications", crate = "::istmo_core", remote = false)]
pub trait Notifications {
    /// What the running backend does natively, simulates or lacks.
    // Backends may ask the notification server (D-Bus, WinRT) once.
    #[istmo_macros::blocking]
    async fn capabilities(&self, ctx: HostCtx) -> Capabilities;

    /// The current permission state, without asking.
    #[istmo_macros::blocking]
    async fn permission(&self, ctx: HostCtx) -> PermissionStatus;

    /// Ask the user. Where there is nothing to ask, the current state.
    #[istmo_macros::blocking]
    async fn request_permission(
        &self,
        ctx: HostCtx,
        request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError>;

    /// Open the system notification settings (of one channel, where the
    /// platform has per-channel pages).
    #[istmo_macros::blocking]
    async fn open_settings(
        &self,
        ctx: HostCtx,
        channel: Option<ChannelId>,
    ) -> Result<(), NotificationError>;

    /// Create or update channels. Existing ones keep their settings
    /// (Android channels are immutable except name and description).
    #[istmo_macros::blocking]
    async fn set_channels(
        &self,
        ctx: HostCtx,
        channels: Vec<ChannelSpec>,
    ) -> Result<(), NotificationError>;

    /// Delete a channel.
    #[istmo_macros::blocking]
    async fn delete_channel(&self, ctx: HostCtx, id: ChannelId) -> Result<(), NotificationError>;

    /// Show now; replaces a notification with the same id.
    #[istmo_macros::blocking]
    async fn show(&self, ctx: HostCtx, notification: Notification)
    -> Result<(), NotificationError>;

    /// Show later, once or repeatedly.
    #[istmo_macros::blocking]
    async fn schedule(
        &self,
        ctx: HostCtx,
        notification: Notification,
        trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError>;

    /// Change a shown notification. Updates are coalesced to the
    /// platform's rate (the latest per id wins) and are silent unless
    /// [`NotificationPatch::alert_again`].
    #[istmo_macros::blocking]
    async fn update(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        patch: NotificationPatch,
    ) -> Result<(), NotificationError>;

    /// Set or remove the progress bar (an update).
    #[istmo_macros::blocking]
    async fn set_progress(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        progress: Option<Progress>,
    ) -> Result<(), NotificationError>;

    /// Append a message to a [`Style::Messaging`] notification (alerts).
    #[istmo_macros::blocking]
    async fn append_message(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        message: ChatMessage,
    ) -> Result<(), NotificationError>;

    /// Remove a shown or scheduled notification. Unknown ids are not an
    /// error.
    #[istmo_macros::blocking]
    async fn cancel(&self, ctx: HostCtx, id: NotificationId) -> Result<(), NotificationError>;

    /// Remove every notification of a group.
    #[istmo_macros::blocking]
    async fn cancel_group(&self, ctx: HostCtx, group: GroupId) -> Result<(), NotificationError>;

    /// Remove every shown and scheduled notification of the app.
    #[istmo_macros::blocking]
    async fn cancel_all(&self, ctx: HostCtx) -> Result<(), NotificationError>;

    /// Scheduled notifications, soonest first.
    #[istmo_macros::blocking]
    async fn pending(&self, ctx: HostCtx) -> Result<Vec<PendingNotification>, NotificationError>;

    /// Shown notifications.
    #[istmo_macros::blocking]
    async fn delivered(
        &self,
        ctx: HostCtx,
    ) -> Result<Vec<DeliveredNotification>, NotificationError>;

    /// A background response is handled: the OS may suspend the app.
    async fn complete(&self, ctx: HostCtx, ack: AckToken) -> Result<(), NotificationError>;

    /// Deliveries, dismissals, snoozes and failures. Events emitted while
    /// nobody listens are kept (the last 64) and replayed to the first
    /// subscriber.
    #[stream]
    fn events(&self) -> NotificationEvent;
}
