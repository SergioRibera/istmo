//! Client sugar: validated calls with typed errors.
//!
//! The generated client methods (`show`, `schedule`, ...) send what they
//! are given and fail with `IstmoError`. The `_checked` versions here
//! validate first, so a malformed notification never leaves the process,
//! and return [`NotificationError`].

use crate::{
    ChatMessage, LiveNotification, Notification, NotificationError, NotificationId,
    NotificationPatch, NotificationsClient, PermissionRequest, PermissionState, PermissionStatus,
    ScheduledInfo, Trigger,
};

impl NotificationsClient {
    /// [`Self::show`] after [`Notification::validate`].
    pub async fn show_checked(&self, notification: Notification) -> Result<(), NotificationError> {
        notification.validate()?;
        self.show(notification)
            .await
            .map_err(NotificationError::from)
    }

    /// Shows a plain notification.
    pub async fn show_simple(
        &self,
        id: &str,
        title: impl Into<String>,
        body: impl Into<String>,
    ) -> Result<(), NotificationError> {
        self.show_checked(Notification::new(id, title)?.with_body(body))
            .await
    }

    /// [`Self::schedule`] after validating both arguments.
    pub async fn schedule_checked(
        &self,
        notification: Notification,
        trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError> {
        notification.validate()?;
        trigger.validate()?;
        self.schedule(notification, trigger)
            .await
            .map_err(NotificationError::from)
    }

    /// [`Self::update`] after [`NotificationPatch::validate`].
    pub async fn update_checked(
        &self,
        id: &str,
        patch: NotificationPatch,
    ) -> Result<(), NotificationError> {
        let id = NotificationId::try_from(id)?;
        patch.validate()?;
        self.update(id, patch)
            .await
            .map_err(NotificationError::from)
    }

    /// [`Self::append_message`] after [`ChatMessage::validate`].
    pub async fn append_message_checked(
        &self,
        id: &str,
        message: ChatMessage,
    ) -> Result<(), NotificationError> {
        let id = NotificationId::try_from(id)?;
        message.validate()?;
        self.append_message(id, message)
            .await
            .map_err(NotificationError::from)
    }

    /// The permission state, asking the user only when it was never
    /// asked ([`PermissionState::NotDetermined`]).
    pub async fn ensure_permission(
        &self,
        request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        let current = self.permission().await.map_err(NotificationError::from)?;
        if current.state != PermissionState::NotDetermined {
            return Ok(current);
        }
        self.request_permission(request)
            .await
            .map_err(NotificationError::from)
    }

    /// Shows `notification` and returns a handle that updates it in place
    /// without awaiting each step. See [`LiveNotification`].
    pub async fn show_live(
        &self,
        notification: Notification,
    ) -> Result<LiveNotification, NotificationError> {
        let live = LiveNotification::new(self, &notification)?;
        if let Err(err) = self.show_checked(notification).await {
            live.detach();
            return Err(err);
        }
        Ok(live)
    }
}
