//! [`FakeNotifications`]: an in-memory host for app tests.

#![expect(
    clippy::significant_drop_tightening,
    reason = "state guards live until the end of short methods on purpose"
)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, Weak};

use istmo_core::sync::lock;
use istmo_core::{HostCtx, IstmoError, Runtime};

use crate::schedule::Engine;
use crate::{
    AckToken, ActionId, Capabilities, ChannelId, ChannelSpec, ChatMessage, DeliveredNotification,
    GroupId, Notification, NotificationError, NotificationEvent, NotificationId, NotificationPatch,
    NotificationResponse, NotificationResponses, Notifications, PendingNotification,
    PermissionRequest, PermissionState, PermissionStatus, Progress, Response, ResponseAction,
    ScheduledInfo, Style, Trigger,
};

/// A call [`FakeNotifications`] received.
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FakeCall {
    /// [`Notifications::request_permission`].
    RequestPermission(PermissionRequest),
    /// [`Notifications::open_settings`].
    OpenSettings(Option<ChannelId>),
    /// [`Notifications::set_channels`].
    SetChannels(Vec<ChannelSpec>),
    /// [`Notifications::delete_channel`].
    DeleteChannel(ChannelId),
    /// [`Notifications::show`].
    Show(Notification),
    /// [`Notifications::schedule`].
    Schedule(Notification, Trigger),
    /// [`Notifications::update`].
    Update(NotificationId, NotificationPatch),
    /// [`Notifications::set_progress`].
    SetProgress(NotificationId, Option<Progress>),
    /// [`Notifications::append_message`].
    AppendMessage(NotificationId, ChatMessage),
    /// [`Notifications::cancel`].
    Cancel(NotificationId),
    /// [`Notifications::cancel_group`].
    CancelGroup(GroupId),
    /// [`Notifications::cancel_all`].
    CancelAll,
    /// [`Notifications::complete`].
    Complete(AckToken),
}

#[derive(Debug)]
struct State {
    capabilities: Capabilities,
    permission: PermissionStatus,
    calls: Vec<FakeCall>,
    shown: BTreeMap<NotificationId, Notification>,
    scheduled: Engine,
    subscribers: Vec<flume::Sender<NotificationEvent>>,
    runtime: Weak<Runtime>,
}

/// A host that records calls and keeps notifications in memory.
///
/// Register it like any host (`NotificationsHost::new(fake.clone())`);
/// clones share state, so the test keeps one to inspect
/// [`calls`](Self::calls) and to feed [`respond`](Self::respond).
/// Validation runs like on the desktop host.
#[derive(Debug, Clone)]
pub struct FakeNotifications {
    state: Arc<Mutex<State>>,
}

impl Default for FakeNotifications {
    /// Everything simulated, permission granted.
    fn default() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                capabilities: Capabilities {
                    backend: "fake".to_owned(),
                    features: Vec::new(),
                    max_actions: 3,
                    max_pending: None,
                    max_active: None,
                },
                permission: PermissionStatus::of(PermissionState::Granted),
                calls: Vec::new(),
                shown: BTreeMap::new(),
                scheduled: Engine::new(jiff::tz::TimeZone::UTC),
                subscribers: Vec::new(),
                runtime: Weak::new(),
            })),
        }
    }
}

impl FakeNotifications {
    /// Reports `capabilities`.
    #[must_use]
    pub fn with_capabilities(self, capabilities: Capabilities) -> Self {
        lock(&self.state).capabilities = capabilities;
        self
    }

    /// Reports `permission` (and grants nothing more on request).
    #[must_use]
    pub fn with_permission(self, permission: PermissionStatus) -> Self {
        lock(&self.state).permission = permission;
        self
    }

    /// Binds the runtime [`respond`](Self::respond) publishes on (calls
    /// bind it too).
    pub fn attach(&self, runtime: &Arc<Runtime>) {
        lock(&self.state).runtime = Arc::downgrade(runtime);
    }

    /// Every call so far.
    #[must_use]
    pub fn calls(&self) -> Vec<FakeCall> {
        lock(&self.state).calls.clone()
    }

    /// Every call so far, emptying the record.
    #[must_use]
    pub fn take_calls(&self) -> Vec<FakeCall> {
        std::mem::take(&mut lock(&self.state).calls)
    }

    /// The notifications shown and not cancelled, with updates applied.
    #[must_use]
    pub fn shown(&self) -> Vec<Notification> {
        lock(&self.state).shown.values().cloned().collect()
    }

    /// Emits `event` on [`Notifications::events`].
    pub fn emit(&self, event: &NotificationEvent) {
        lock(&self.state)
            .subscribers
            .retain(|tx| tx.try_send(event.clone()).is_ok() || !tx.is_disconnected());
    }

    /// Publishes the response `kind` to notification `id` on the
    /// responses lane, as a platform would. `Action` / `Reply` name an
    /// action id; the response copies the shown notification's user data
    /// and URL.
    ///
    /// # Errors
    ///
    /// An invalid `id`, no runtime bound
    /// ([`IstmoError::RuntimeShutDown`]), or a codec error.
    pub fn respond(
        &self,
        id: &str,
        kind: Response<'_>,
    ) -> Result<NotificationResponse, NotificationError> {
        let id = NotificationId::try_from(id)?;
        let (runtime, notification) = {
            let state = lock(&self.state);
            (state.runtime.upgrade(), state.shown.get(&id).cloned())
        };
        let runtime = runtime.ok_or(IstmoError::RuntimeShutDown)?;
        let button = |action: &str| {
            ResponseAction::Button(ActionId {
                value: action.to_owned(),
            })
        };
        let (action, reply) = match kind {
            Response::Open => (ResponseAction::Open, None),
            Response::Action(action) => (button(action), None),
            Response::Reply(text) => {
                let reply_action = notification
                    .as_ref()
                    .and_then(|n| {
                        n.actions
                            .iter()
                            .find(|a| matches!(a.kind, crate::ActionKind::Reply(_)))
                    })
                    .map_or_else(|| "reply".to_owned(), |a| a.id.value.clone());
                (button(&reply_action), Some(text.to_owned()))
            }
            Response::Dismissed => (ResponseAction::Dismiss, None),
            Response::More => (ResponseAction::More, None),
        };
        let response = NotificationResponse {
            id,
            action,
            reply,
            input_requested: false,
            user_data: notification.as_ref().and_then(|n| n.user_data.clone()),
            url: notification.as_ref().and_then(|n| n.url.clone()),
            launched_app: false,
            overflow_actions: Vec::new(),
            ack: None,
        };
        NotificationResponses::publish(&runtime, &response)?;
        Ok(response)
    }

    fn record(&self, ctx: &HostCtx, call: FakeCall) {
        let mut state = lock(&self.state);
        if let Ok(runtime) = ctx.runtime() {
            state.runtime = Arc::downgrade(&runtime);
        }
        state.calls.push(call);
    }
}

impl Notifications for FakeNotifications {
    async fn capabilities(&self, _ctx: HostCtx) -> Capabilities {
        lock(&self.state).capabilities.clone()
    }

    async fn permission(&self, _ctx: HostCtx) -> PermissionStatus {
        lock(&self.state).permission
    }

    async fn request_permission(
        &self,
        ctx: HostCtx,
        request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        self.record(&ctx, FakeCall::RequestPermission(request));
        Ok(lock(&self.state).permission)
    }

    async fn open_settings(
        &self,
        ctx: HostCtx,
        channel: Option<ChannelId>,
    ) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::OpenSettings(channel));
        Ok(())
    }

    async fn set_channels(
        &self,
        ctx: HostCtx,
        channels: Vec<ChannelSpec>,
    ) -> Result<(), NotificationError> {
        for channel in &channels {
            channel.validate()?;
        }
        self.record(&ctx, FakeCall::SetChannels(channels));
        Ok(())
    }

    async fn delete_channel(&self, ctx: HostCtx, id: ChannelId) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::DeleteChannel(id));
        Ok(())
    }

    async fn show(
        &self,
        ctx: HostCtx,
        notification: Notification,
    ) -> Result<(), NotificationError> {
        notification.validate()?;
        self.record(&ctx, FakeCall::Show(notification.clone()));
        let id = notification.id.clone();
        lock(&self.state).shown.insert(id.clone(), notification);
        self.emit(&NotificationEvent::Delivered(id));
        Ok(())
    }

    async fn schedule(
        &self,
        ctx: HostCtx,
        notification: Notification,
        trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError> {
        notification.validate()?;
        trigger.validate()?;
        self.record(
            &ctx,
            FakeCall::Schedule(notification.clone(), trigger.clone()),
        );
        let now = crate::builder::now_ms();
        Ok(lock(&self.state).scheduled.add(notification, trigger, now))
    }

    async fn update(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        patch: NotificationPatch,
    ) -> Result<(), NotificationError> {
        patch.validate()?;
        self.record(&ctx, FakeCall::Update(id.clone(), patch.clone()));
        let mut state = lock(&self.state);
        let shown = state
            .shown
            .get_mut(&id)
            .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
        shown.apply(patch);
        Ok(())
    }

    async fn set_progress(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        progress: Option<Progress>,
    ) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::SetProgress(id.clone(), progress.clone()));
        let mut state = lock(&self.state);
        let shown = state
            .shown
            .get_mut(&id)
            .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
        shown.progress = progress;
        Ok(())
    }

    async fn append_message(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        message: ChatMessage,
    ) -> Result<(), NotificationError> {
        message.validate()?;
        self.record(&ctx, FakeCall::AppendMessage(id.clone(), message.clone()));
        let mut state = lock(&self.state);
        let shown = state
            .shown
            .get_mut(&id)
            .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
        let Style::Messaging(conversation) = &mut shown.style else {
            return Err(crate::InvalidField::new("style", crate::Problem::NotAllowedHere).into());
        };
        conversation.messages.push(message);
        shown.normalize();
        Ok(())
    }

    async fn cancel(&self, ctx: HostCtx, id: NotificationId) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::Cancel(id.clone()));
        let mut state = lock(&self.state);
        state.shown.remove(&id);
        state.scheduled.remove(&id);
        Ok(())
    }

    async fn cancel_group(&self, ctx: HostCtx, group: GroupId) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::CancelGroup(group.clone()));
        let mut state = lock(&self.state);
        state
            .shown
            .retain(|_, n| n.group.as_ref().is_none_or(|g| g.id != group));
        state.scheduled.remove_group(&group);
        Ok(())
    }

    async fn cancel_all(&self, ctx: HostCtx) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::CancelAll);
        let mut state = lock(&self.state);
        state.shown.clear();
        state.scheduled.clear();
        Ok(())
    }

    async fn pending(&self, _ctx: HostCtx) -> Result<Vec<PendingNotification>, NotificationError> {
        Ok(lock(&self.state).scheduled.pending())
    }

    async fn delivered(
        &self,
        _ctx: HostCtx,
    ) -> Result<Vec<DeliveredNotification>, NotificationError> {
        Ok(lock(&self.state)
            .shown
            .values()
            .map(|n| DeliveredNotification {
                id: n.id.clone(),
                title: Some(n.title.clone()),
                body: n.body.clone(),
                group: n.group.as_ref().map(|g| g.id.clone()),
                delivered_ms: None,
            })
            .collect())
    }

    async fn complete(&self, ctx: HostCtx, ack: AckToken) -> Result<(), NotificationError> {
        self.record(&ctx, FakeCall::Complete(ack));
        Ok(())
    }

    fn events(&self) -> flume::Receiver<NotificationEvent> {
        let (tx, rx) = flume::bounded(64);
        lock(&self.state).subscribers.push(tx);
        rx
    }
}
