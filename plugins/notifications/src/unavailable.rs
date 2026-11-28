use istmo_core::HostCtx;

use crate::{
    AckToken, Capabilities, ChannelId, ChannelSpec, ChatMessage, DeliveredNotification, Feature,
    GroupId, Notification, NotificationError, NotificationEvent, NotificationId, NotificationPatch,
    Notifications, PendingNotification, PermissionRequest, PermissionState, PermissionStatus,
    Progress, ScheduledInfo, Trigger,
};

/// Host for targets without notifications.
///
/// [`Notifications::permission`]
/// reports [`PermissionState::Unavailable`], every call fails with
/// [`NotificationError::Unsupported`], the event stream ends at once.
///
/// ```rust,ignore
/// runtime.register_host(NotificationsHost::new(UnavailableNotifications));
/// ```
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableNotifications;

impl Capabilities {
    /// A report where nothing is supported.
    #[must_use]
    pub fn none(backend: impl Into<String>) -> Self {
        Self {
            backend: backend.into(),
            features: Vec::new(),
            max_actions: 0,
            max_pending: Some(0),
            max_active: Some(0),
        }
    }
}

const fn unsupported<T>(feature: Feature) -> Result<T, NotificationError> {
    Err(NotificationError::Unsupported(feature))
}

impl Notifications for UnavailableNotifications {
    async fn capabilities(&self, _ctx: HostCtx) -> Capabilities {
        Capabilities::none("unavailable")
    }

    async fn permission(&self, _ctx: HostCtx) -> PermissionStatus {
        PermissionStatus::of(PermissionState::Unavailable)
    }

    async fn request_permission(
        &self,
        _ctx: HostCtx,
        _request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        Ok(PermissionStatus::of(PermissionState::Unavailable))
    }

    async fn open_settings(
        &self,
        _ctx: HostCtx,
        _channel: Option<ChannelId>,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::OpenSettings)
    }

    async fn set_channels(
        &self,
        _ctx: HostCtx,
        _channels: Vec<ChannelSpec>,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::Channels)
    }

    async fn delete_channel(&self, _ctx: HostCtx, _id: ChannelId) -> Result<(), NotificationError> {
        unsupported(Feature::Channels)
    }

    async fn show(
        &self,
        _ctx: HostCtx,
        _notification: Notification,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::Show)
    }

    async fn schedule(
        &self,
        _ctx: HostCtx,
        _notification: Notification,
        _trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError> {
        unsupported(Feature::Schedule)
    }

    async fn update(
        &self,
        _ctx: HostCtx,
        _id: NotificationId,
        _patch: NotificationPatch,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::Update)
    }

    async fn set_progress(
        &self,
        _ctx: HostCtx,
        _id: NotificationId,
        _progress: Option<Progress>,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::Update)
    }

    async fn append_message(
        &self,
        _ctx: HostCtx,
        _id: NotificationId,
        _message: ChatMessage,
    ) -> Result<(), NotificationError> {
        unsupported(Feature::Messaging)
    }

    async fn cancel(&self, _ctx: HostCtx, _id: NotificationId) -> Result<(), NotificationError> {
        unsupported(Feature::Show)
    }

    async fn cancel_group(&self, _ctx: HostCtx, _group: GroupId) -> Result<(), NotificationError> {
        unsupported(Feature::Groups)
    }

    async fn cancel_all(&self, _ctx: HostCtx) -> Result<(), NotificationError> {
        unsupported(Feature::Show)
    }

    async fn pending(&self, _ctx: HostCtx) -> Result<Vec<PendingNotification>, NotificationError> {
        unsupported(Feature::ListPending)
    }

    async fn delivered(
        &self,
        _ctx: HostCtx,
    ) -> Result<Vec<DeliveredNotification>, NotificationError> {
        unsupported(Feature::ListDelivered)
    }

    async fn complete(&self, _ctx: HostCtx, _ack: AckToken) -> Result<(), NotificationError> {
        unsupported(Feature::BackgroundAction)
    }

    fn events(&self) -> flume::Receiver<NotificationEvent> {
        // The sender is dropped here: the stream ends at once.
        flume::bounded(1).1
    }
}
