//! Desktop host: [`DesktopNotifications`] over a platform [`Backend`].
//!
//! Everything that is not the platform lives here, once for Linux,
//! Windows and macOS: validation, channel defaults, render fallbacks,
//! live-update coalescing, the in-process scheduler and its store, the
//! delivered table, image files, the event hub and the responses lane.
//!
//! Platform backends implement [`Backend`]. Until a platform has one,
//! [`DesktopNotifications::default`] answers like
//! [`crate::UnavailableNotifications`] (its backend fails to start with
//! [`NotificationError::Unsupported`]).
//!
//! Locks: the host never holds a lock while it calls the backend, and
//! takes the state lock before the image cache, never the other way.

#![expect(
    clippy::significant_drop_tightening,
    reason = "state guards live in short blocks that end right after their last use; the lint asks for explicit drops there"
)]

mod backend;
mod images;
mod timer;

#[cfg(test)]
mod tests;

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use istmo_core::sync::lock;
use istmo_core::{HostCtx, Runtime};

pub use backend::{
    Activation, Backend, BackendInfo, BackendSink, Carried, ImageFiles, ImageSlot, NullBackend,
    ShowMode, Shown,
};

use crate::coalesce::{Admit, Coalescer, Rate, Urgency};
use crate::render::{Labels, RenderLimits, Rendered, Resolved, render};
use crate::schedule::{Engine, Store, Wakeup};
use crate::{
    AckToken, ActionId, Capabilities, ChannelId, ChannelSpec, ChatMessage, DeliveredNotification,
    DismissReason, Dismissal, Failure, Feature, FeatureSupport, GroupId, InvalidField,
    Notification, NotificationError, NotificationEvent, NotificationId, NotificationPatch,
    NotificationResponse, NotificationResponses, Notifications, PatchField, PendingNotification,
    PermissionRequest, PermissionState, PermissionStatus, Problem, Progress, ResponseAction,
    ScheduledInfo, Style, Support, Trigger,
};
use images::ImageCache;
use timer::{HostTimer, ManualTimer, Tick, Timer};

/// Events kept for the first subscriber.
const BACKLOG: usize = 64;
/// Depth of each subscriber's channel.
const SUBSCRIBER_DEPTH: usize = 256;
/// Dismissed notifications kept (for an alerting update to show them
/// again); older ones are forgotten.
const MAX_DISMISSED: usize = 256;

/// Starts the platform backend, handing it the sink it reports through.
pub type StartBackend =
    Box<dyn Fn(BackendSink) -> Result<Arc<dyn Backend>, NotificationError> + Send + Sync>;

/// The platform backend of this target. None is built in yet: platform
/// modules replace this.
fn platform_backend(_sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    Err(NotificationError::Unsupported(Feature::Show))
}

/// A shown notification as the host keeps it.
#[derive(Debug)]
struct Live {
    /// Latest content (queued updates included).
    notification: Notification,
    /// What the backend last got.
    rendered: Rendered,
    images: ImageFiles,
    /// The images must be collected again before the next post.
    images_stale: bool,
    delivered_ms: i64,
    /// Gone from the screen; kept for an alerting update.
    dismissed: bool,
    /// Removal deadline when the host runs the timeout.
    expires_ms: Option<i64>,
}

#[derive(Debug)]
struct State {
    table: HashMap<NotificationId, Live>,
    /// Dismissed ids, oldest first.
    dismissed: VecDeque<NotificationId>,
    coalescer: Coalescer<()>,
    engine: Engine,
    channels: HashMap<ChannelId, ChannelSpec>,
    acks: HashMap<u64, NotificationId>,
    next_ack: u64,
    /// Store loaded and missed deliveries applied.
    loaded: bool,
    /// Bumped on every engine change; the store keeps the newest.
    store_generation: u64,
}

/// Subscribers of the events, with a backlog until the first one.
#[derive(Debug, Default)]
struct EventHub {
    backlog: VecDeque<NotificationEvent>,
    subscribers: Vec<flume::Sender<NotificationEvent>>,
    subscribed: bool,
}

impl EventHub {
    fn emit(&mut self, event: NotificationEvent) {
        if !self.subscribed {
            if self.backlog.len() == BACKLOG {
                self.backlog.pop_front();
            }
            self.backlog.push_back(event);
            return;
        }
        self.subscribers
            .retain(|tx| match tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(flume::TrySendError::Full(_)) => {
                    tracing::warn!(
                        "notifications: an event subscriber is not reading; event dropped"
                    );
                    true
                }
                Err(flume::TrySendError::Disconnected(_)) => false,
            });
    }

    fn subscribe(&mut self) -> flume::Receiver<NotificationEvent> {
        let (tx, rx) = flume::bounded(SUBSCRIBER_DEPTH);
        if !self.subscribed {
            self.subscribed = true;
            for event in self.backlog.drain(..) {
                let _queued = tx.try_send(event);
            }
        }
        self.subscribers.push(tx);
        rx
    }
}

/// The shared host behind every clone of [`DesktopNotifications`].
pub(crate) struct Inner {
    start: StartBackend,
    backend: OnceLock<Result<Arc<dyn Backend>, NotificationError>>,
    runtime: OnceLock<Weak<Runtime>>,
    state: Mutex<State>,
    events: Mutex<EventHub>,
    images: Mutex<ImageCache>,
    store: Option<Store>,
    /// Serialises store writes; holds the generation last written.
    store_written: Mutex<u64>,
    timer: OnceLock<HostTimer<Self>>,
    clock: fn() -> i64,
    labels: Labels,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopNotifications")
            .field("store", &self.store)
            .finish_non_exhaustive()
    }
}

/// The notifications host for Linux, Windows and macOS.
///
/// Register it with the runtime
/// (`hosts: [NotificationsClient => DesktopNotifications::default()]`).
/// Clones share one host: keep one to [`attach`](Self::attach) it to the
/// runtime at start, so missed scheduled notifications are delivered and
/// taps that started the process reach the app before its first call.
///
/// Scheduling runs in-process: deliveries happen while the app runs
/// ([`Support::WhileRunning`]); entries persist in the store and missed
/// ones follow their [`crate::MissedPolicy`] at the next start.
#[derive(Debug, Clone)]
pub struct DesktopNotifications {
    inner: Arc<Inner>,
}

impl Default for DesktopNotifications {
    /// The platform backend, the store at
    /// `<data dir>/istmo-notifications/schedule.bin`, images under
    /// `<cache dir>/istmo-notifications/images`.
    fn default() -> Self {
        Self::builder(Box::new(platform_backend)).build()
    }
}

/// Configures a [`DesktopNotifications`].
pub struct DesktopNotificationsBuilder {
    start: StartBackend,
    store: Option<PathBuf>,
    images: PathBuf,
    channels: Vec<ChannelSpec>,
    labels: Labels,
    clock: fn() -> i64,
    manual_timer: bool,
}

impl std::fmt::Debug for DesktopNotificationsBuilder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopNotificationsBuilder")
            .field("store", &self.store)
            .field("images", &self.images)
            .finish_non_exhaustive()
    }
}

impl DesktopNotificationsBuilder {
    /// Keeps scheduled notifications in `path`.
    #[must_use]
    pub fn store_at(mut self, path: impl Into<PathBuf>) -> Self {
        self.store = Some(path.into());
        self
    }

    /// Keeps scheduled notifications in memory only. Use it in every
    /// process but the one owning the app's single-instance endpoint
    /// (one writer per store).
    #[must_use]
    pub fn without_store(mut self) -> Self {
        self.store = None;
        self
    }

    /// Writes image files under `dir`.
    #[must_use]
    pub fn images_in(mut self, dir: impl Into<PathBuf>) -> Self {
        self.images = dir.into();
        self
    }

    /// Channels known from the start (`[[app.notification_channel]]`).
    #[must_use]
    pub fn with_channels(mut self, channels: Vec<ChannelSpec>) -> Self {
        self.channels = channels;
        self
    }

    /// Labels of the actions the plugin adds ("More", call buttons).
    #[must_use]
    pub fn with_labels(mut self, labels: Labels) -> Self {
        self.labels = labels;
        self
    }

    /// The wall clock, Unix-epoch milliseconds (tests).
    #[must_use]
    pub fn with_clock(mut self, clock: fn() -> i64) -> Self {
        self.clock = clock;
        self
    }

    /// No timer thread: deadlines wait for [`DesktopNotifications::tick`]
    /// (tests).
    #[must_use]
    pub const fn manual_timer(mut self) -> Self {
        self.manual_timer = true;
        self
    }

    /// The host.
    #[must_use]
    pub fn build(self) -> DesktopNotifications {
        let channels = self
            .channels
            .into_iter()
            .map(|channel| (channel.id.clone(), channel))
            .collect();
        let now = (self.clock)();
        let inner = Arc::new(Inner {
            start: self.start,
            backend: OnceLock::new(),
            runtime: OnceLock::new(),
            state: Mutex::new(State {
                table: HashMap::new(),
                dismissed: VecDeque::new(),
                coalescer: Coalescer::new(Rate::LINUX, now),
                engine: Engine::default(),
                channels,
                acks: HashMap::new(),
                next_ack: 1,
                loaded: false,
                store_generation: 0,
            }),
            events: Mutex::default(),
            images: Mutex::new(ImageCache::new(self.images)),
            store: self.store.map(Store::new),
            store_written: Mutex::new(0),
            timer: OnceLock::new(),
            clock: self.clock,
            labels: self.labels,
        });
        let timer = if self.manual_timer {
            HostTimer::Manual(Arc::new(ManualTimer::default()))
        } else {
            HostTimer::Thread(Timer::new(Arc::downgrade(&inner)))
        };
        let _set = inner.timer.set(timer);
        DesktopNotifications { inner }
    }
}

impl DesktopNotifications {
    /// A host over the backend `start` returns (called once, on first
    /// use, with the sink the backend reports through).
    #[must_use]
    pub fn builder(start: StartBackend) -> DesktopNotificationsBuilder {
        let base = Path::new("istmo-notifications");
        DesktopNotificationsBuilder {
            start,
            store: Some(istmo_core::path::data_dir().join(base).join("schedule.bin")),
            images: istmo_core::path::cache_dir().join(base).join("images"),
            channels: Vec::new(),
            labels: Labels::default(),
            clock: crate::builder::now_ms,
            manual_timer: false,
        }
    }

    /// A host over an already built `backend` (custom backends, tests).
    #[must_use]
    pub fn with_backend(backend: Arc<dyn Backend>) -> DesktopNotificationsBuilder {
        Self::builder(Box::new(move |_sink| Ok(Arc::clone(&backend))))
    }

    /// Binds the host to `runtime` now: starts the backend, loads the
    /// store and delivers missed scheduled notifications. Without it this
    /// happens on the first call.
    pub fn attach(&self, runtime: &Arc<Runtime>) {
        self.inner.bind(runtime);
    }

    /// Runs what is due at the host clock's now (scheduled deliveries,
    /// queued updates, timeouts); returns the next deadline. The timer
    /// thread calls it; hosts built with
    /// [`DesktopNotificationsBuilder::manual_timer`] wait for the caller.
    #[expect(
        clippy::must_use_candidate,
        reason = "called for its effects; the deadline is extra"
    )]
    pub fn tick(&self) -> Option<i64> {
        self.inner.run_due()
    }

    /// The deadline armed last, with a manual timer.
    #[must_use]
    pub fn armed(&self) -> Option<i64> {
        match self.inner.timer.get() {
            Some(HostTimer::Manual(timer)) => *lock(&timer.armed),
            _ => None,
        }
    }

    /// A sink reporting to this host.
    #[must_use]
    pub fn sink(&self) -> BackendSink {
        BackendSink {
            host: Arc::downgrade(&self.inner),
        }
    }
}

/// Notifications in a known channel get the channel's defaults.
fn merge_channel(
    n: &mut Notification,
    channels: &HashMap<ChannelId, ChannelSpec>,
) -> Result<(), NotificationError> {
    let Some(id) = &n.channel else {
        return Ok(());
    };
    let Some(channel) = channels.get(id) else {
        return Err(NotificationError::UnknownChannel(id.value.clone()));
    };
    if n.interruption.is_none() {
        n.interruption = Some(channel.importance);
    }
    if n.sound.is_none() {
        n.sound.clone_from(&channel.sound);
    }
    if n.lock_screen.is_none() {
        n.lock_screen = channel.lock_screen;
    }
    Ok(())
}

/// The backend's report with the host's simulations added.
fn overlay(info: &BackendInfo) -> Capabilities {
    let mut caps = info.capabilities.clone();
    let mut set = |feature: Feature, support: Support| match caps
        .features
        .iter_mut()
        .find(|entry| entry.feature == feature)
    {
        Some(entry) if entry.support == Support::Unsupported => entry.support = support,
        Some(_) => {}
        None => caps.features.push(FeatureSupport { feature, support }),
    };
    set(Feature::Schedule, Support::WhileRunning);
    set(Feature::Repeat, Support::WhileRunning);
    set(Feature::Missed, Support::Simulated);
    set(Feature::ListPending, Support::Simulated);
    set(Feature::Snooze, Support::WhileRunning);
    set(Feature::Channels, Support::Simulated);
    set(Feature::ProgressBar, Support::Simulated);
    set(Feature::Messaging, Support::Simulated);
    if !info.lists_delivered {
        set(Feature::ListDelivered, Support::WhileRunning);
    }
    if !info.timeouts {
        set(Feature::Timeout, Support::WhileRunning);
    }
    if info.capabilities.support(Feature::Actions) != Support::Unsupported {
        set(Feature::InlineReply, Support::Simulated);
        set(Feature::Choices, Support::Simulated);
        set(Feature::CallStyle, Support::Simulated);
    }
    caps
}

/// Removes `id` from the dismissed order.
fn forget_dismissed(state: &mut State, id: &NotificationId) {
    if let Some(at) = state.dismissed.iter().position(|seen| seen == id) {
        state.dismissed.remove(at);
    }
}

/// The action id of a rendered key (`<id>#<choice>` for choices).
fn action_of(key: &str) -> &str {
    key.split_once('#').map_or(key, |(action, _)| action)
}

/// What a post needs, prepared under the state lock.
struct Post {
    notification: Notification,
    rendered: Rendered,
    images: ImageFiles,
    /// The images are new: released if the post fails.
    fresh: bool,
    /// Images of the previous content, released once the post went out.
    old_images: Option<ImageFiles>,
    mode: ShowMode,
}

impl Inner {
    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn backend(self: &Arc<Self>) -> Result<&Arc<dyn Backend>, NotificationError> {
        self.backend
            .get_or_init(|| {
                (self.start)(BackendSink {
                    host: Arc::downgrade(self),
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    fn emit(&self, event: NotificationEvent) {
        lock(&self.events).emit(event);
    }

    fn runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime.get().and_then(Weak::upgrade)
    }

    /// Binds the runtime of the first call (and loads once).
    fn bind_ctx(self: &Arc<Self>, ctx: &HostCtx) {
        if self.runtime.get().is_none()
            && let Ok(runtime) = ctx.runtime()
        {
            self.bind(&runtime);
        }
    }

    fn bind(self: &Arc<Self>, runtime: &Arc<Runtime>) {
        let _set = self.runtime.set(Arc::downgrade(runtime));
        self.load();
    }

    /// Loads the store, sweeps stale images and delivers what was missed.
    fn load(self: &Arc<Self>) {
        {
            let mut state = self.lock();
            if state.loaded {
                return;
            }
            state.loaded = true;
        }
        let stored = match self.store.as_ref().map(Store::load) {
            Some(Ok(entries)) => entries,
            Some(Err(err)) => {
                tracing::warn!(%err, "notifications: schedule store unreadable; starting empty");
                Vec::new()
            }
            None => Vec::new(),
        };
        lock(&self.images).sweep(std::time::SystemTime::now());
        let rate = self.backend().map(|backend| backend.info().rate).ok();
        {
            let mut state = self.lock();
            // Entries scheduled before the load replace stored ones with
            // the same id.
            let mut engine = Engine::with_entries(jiff::tz::TimeZone::system(), stored);
            for entry in state.engine.entries() {
                engine.add(
                    entry.notification.clone(),
                    entry.trigger.clone(),
                    entry.anchor_ms,
                );
            }
            state.engine = engine;
            if let Some(rate) = rate {
                state.coalescer = Coalescer::new(rate, (self.clock)());
            }
        }
        self.run_due();
    }

    /// Writes the engine's entries if they changed since the last write.
    fn persist(&self) {
        let Some(store) = &self.store else {
            return;
        };
        let (generation, bytes) = {
            let state = self.lock();
            let bytes = if state.engine.is_empty() {
                Ok(None)
            } else {
                Store::encode(state.engine.entries()).map(Some)
            };
            (state.store_generation, bytes)
        };
        let mut written = lock(&self.store_written);
        if *written >= generation {
            return;
        }
        match bytes.and_then(|bytes| store.write(bytes.as_deref())) {
            Ok(()) => *written = generation,
            Err(err) => tracing::warn!(%err, "notifications: schedule store not written"),
        }
    }

    const fn changed_engine(state: &mut State) {
        state.store_generation += 1;
    }

    /// The next deadline of everything the host waits for.
    fn next_deadline(state: &State) -> Option<i64> {
        let expiry = state
            .table
            .values()
            .filter_map(|live| live.expires_ms)
            .min();
        [
            state.engine.next_deadline(),
            state.coalescer.next_due(),
            expiry,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    fn rearm(&self) -> Option<i64> {
        let deadline = Self::next_deadline(&self.lock());
        if let Some(timer) = self.timer.get() {
            timer.arm(deadline);
        }
        deadline
    }

    fn limits(&self, info: &BackendInfo) -> RenderLimits {
        let mut limits = info.limits.clone();
        limits.labels.clone_from(&self.labels);
        limits
    }

    /// Prepares the post of `notification` (validated, normalised) under
    /// the state lock.
    fn prepare(
        &self,
        state: &State,
        limits: &RenderLimits,
        notification: Notification,
        mode: ShowMode,
    ) -> Post {
        let rendered = render(&notification, limits);
        let current = state
            .table
            .get(&notification.id)
            .filter(|live| !live.dismissed);
        let (images, fresh, old_images) = match current {
            Some(live) if !live.images_stale => (live.images.clone(), false, None),
            current => (
                lock(&self.images).collect(&notification),
                true,
                current.map(|live| live.images.clone()),
            ),
        };
        Post {
            notification,
            rendered,
            images,
            fresh,
            old_images,
            mode,
        }
    }

    /// Sends a prepared post and records it.
    fn post(
        &self,
        backend: &dyn Backend,
        info: &BackendInfo,
        post: Post,
    ) -> Result<(), NotificationError> {
        let result = backend.show(&Shown {
            notification: &post.notification,
            rendered: &post.rendered,
            images: &post.images,
            mode: post.mode,
        });
        if let Err(err) = result {
            if post.fresh {
                lock(&self.images).release(&post.images);
            }
            return Err(err);
        }
        let now = self.now();
        let id = post.notification.id.clone();
        let expires_ms = if info.timeouts {
            None
        } else {
            post.notification
                .timeout_ms
                .map(|ms| now.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)))
        };
        let mut state = self.lock();
        let delivered_ms = state
            .table
            .get(&id)
            .filter(|live| !live.dismissed && post.mode != ShowMode::New)
            .map_or(now, |live| live.delivered_ms);
        forget_dismissed(&mut state, &id);
        state.table.insert(
            id,
            Live {
                notification: post.notification,
                rendered: post.rendered,
                images: post.images,
                images_stale: false,
                delivered_ms,
                dismissed: false,
                expires_ms,
            },
        );
        drop(state);
        if let Some(old) = &post.old_images {
            lock(&self.images).release(old);
        }
        Ok(())
    }

    /// A full show: replaces whatever has this id.
    fn show_now(
        self: &Arc<Self>,
        mut notification: Notification,
        late: bool,
    ) -> Result<(), NotificationError> {
        notification.validate()?;
        notification.normalize();
        let backend = Arc::clone(self.backend()?);
        let info = backend.info();
        let limits = self.limits(&info);
        let now = self.now();
        let post = {
            let mut state = self.lock();
            merge_channel(&mut notification, &state.channels)?;
            let shown = state
                .table
                .get(&notification.id)
                .is_some_and(|live| !live.dismissed);
            state.coalescer.shown(&notification.id, now);
            let mode = if shown {
                ShowMode::Replace { alert: true }
            } else {
                ShowMode::New
            };
            self.prepare(&state, &limits, notification, mode)
        };
        let id = post.notification.id.clone();
        self.post(backend.as_ref(), &info, post)?;
        self.emit(if late {
            NotificationEvent::DeliveredLate(id)
        } else {
            NotificationEvent::Delivered(id)
        });
        self.rearm();
        Ok(())
    }

    /// Changes the shown `id` with `change`; posts through the coalescer.
    fn change(
        self: &Arc<Self>,
        id: &NotificationId,
        urgency: Urgency,
        change: impl FnOnce(&mut Live) -> Result<(), NotificationError>,
    ) -> Result<(), NotificationError> {
        let backend = Arc::clone(self.backend()?);
        let info = backend.info();
        let limits = self.limits(&info);
        let now = self.now();
        let post = {
            let mut state = self.lock();
            let live = state
                .table
                .get_mut(id)
                .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
            let before = live.notification.clone();
            change(live)?;
            if let Err(err) = live.notification.validate() {
                live.notification = before;
                return Err(err.into());
            }
            live.notification.normalize();
            let was_dismissed = live.dismissed;
            match state.coalescer.offer(id, (), urgency, now) {
                Admit::Dropped | Admit::Queued => None,
                Admit::Now(()) => {
                    let notification = state.table[id].notification.clone();
                    let mode = if was_dismissed {
                        ShowMode::New
                    } else {
                        ShowMode::Replace {
                            alert: urgency != Urgency::Regular,
                        }
                    };
                    Some(self.prepare(&state, &limits, notification, mode))
                }
            }
        };
        if let Some(post) = post {
            self.post(backend.as_ref(), &info, post)?;
        }
        self.rearm();
        Ok(())
    }

    /// Takes `ids` off the screen and out of the schedule.
    fn remove_ids(self: &Arc<Self>, ids: &[NotificationId]) -> Result<(), NotificationError> {
        let mut shown = Vec::with_capacity(ids.len());
        let mut released = Vec::new();
        {
            let mut state = self.lock();
            let mut engine_changed = false;
            for id in ids {
                state.coalescer.cancel(id);
                engine_changed |= state.engine.remove(id);
                forget_dismissed(&mut state, id);
                if let Some(live) = state.table.remove(id)
                    && !live.dismissed
                {
                    shown.push(id.clone());
                    released.push(live.images);
                }
                state.acks.retain(|_, acked| acked != id);
            }
            if engine_changed {
                Self::changed_engine(&mut state);
            }
        }
        {
            let mut cache = lock(&self.images);
            for files in &released {
                cache.release(files);
            }
        }
        self.persist();
        let mut first_error = None;
        if !shown.is_empty() {
            let backend = Arc::clone(self.backend()?);
            for id in &shown {
                if let Err(err) = backend.remove(id) {
                    first_error.get_or_insert(err);
                }
            }
        }
        self.rearm();
        first_error.map_or(Ok(()), Err)
    }

    /// Runs what is due: scheduled deliveries, queued updates, timeouts.
    fn run_due(self: &Arc<Self>) -> Option<i64> {
        let now = self.now();
        let (due, queued, expired) = {
            let mut state = self.lock();
            if !state.loaded {
                return None;
            }
            let due = state.engine.due(now);
            if !due.is_empty() {
                Self::changed_engine(&mut state);
            }
            let queued: Vec<NotificationId> = state
                .coalescer
                .poll(now)
                .into_iter()
                .map(|(id, ())| id)
                .collect();
            let expired: Vec<NotificationId> = state
                .table
                .iter()
                .filter(|(_, live)| !live.dismissed && live.expires_ms.is_some_and(|at| at <= now))
                .map(|(id, _)| id.clone())
                .collect();
            (due, queued, expired)
        };
        if !due.is_empty() {
            self.persist();
        }
        for entry in due {
            let id = entry.notification.id.clone();
            if let Err(err) = self.show_now(entry.notification, entry.late) {
                self.failed(Some(&id), &err.to_string());
            }
        }
        if !queued.is_empty()
            && let Ok(backend) = self.backend().map(Arc::clone)
        {
            let info = backend.info();
            let limits = self.limits(&info);
            for id in queued {
                let post = {
                    let state = self.lock();
                    let Some(live) = state.table.get(&id).filter(|live| !live.dismissed) else {
                        continue;
                    };
                    let notification = live.notification.clone();
                    self.prepare(
                        &state,
                        &limits,
                        notification,
                        ShowMode::Replace { alert: false },
                    )
                };
                if let Err(err) = self.post(backend.as_ref(), &info, post) {
                    self.failed(Some(&id), &err.to_string());
                }
            }
        }
        for id in expired {
            if self.remove_ids(std::slice::from_ref(&id)).is_ok() {
                self.emit(NotificationEvent::Dismissed(Dismissal {
                    id,
                    reason: DismissReason::Timeout,
                }));
            }
        }
        self.rearm()
    }

    /// A platform activation.
    fn activated(self: &Arc<Self>, activation: Activation) {
        let Activation {
            id,
            key,
            reply,
            launched_app,
            carried,
        } = activation;
        let resolved = {
            let state = self.lock();
            state.table.get(&id).map(|live| {
                let background = key.as_deref().is_some_and(|key| {
                    live.notification
                        .actions
                        .iter()
                        .any(|a| a.id.as_str() == action_of(key) && !a.opens_app)
                });
                let resolved = live.rendered.respond(
                    &live.notification,
                    key.as_deref(),
                    reply.clone(),
                    launched_app,
                );
                (resolved, background, live.notification.clone())
            })
        };
        let response = match resolved {
            Some((Some(Resolved::Snooze(ms)), _, notification)) => {
                self.snooze(notification, ms);
                return;
            }
            Some((Some(Resolved::Respond(response)), background, _)) => {
                self.with_ack(response, background)
            }
            Some((None, ..)) => {
                tracing::debug!(id = %id, "notifications: activation with an unknown action key");
                return;
            }
            // Shown by an earlier process: what the platform carried.
            None => {
                let carried = carried.unwrap_or_default();
                let action = match key.as_deref() {
                    None => ResponseAction::Open,
                    Some(ActionId::MORE) => ResponseAction::More,
                    Some(key) => ResponseAction::Button(ActionId {
                        value: action_of(key).to_owned(),
                    }),
                };
                NotificationResponse {
                    id,
                    action,
                    reply,
                    input_requested: false,
                    user_data: carried.user_data,
                    url: carried.url,
                    launched_app,
                    overflow_actions: Vec::new(),
                    ack: None,
                }
            }
        };
        match self.runtime() {
            Some(runtime) => {
                if let Err(err) = NotificationResponses::publish(&runtime, &response) {
                    tracing::warn!(%err, "notifications: response not published");
                }
            }
            None => {
                tracing::warn!("notifications: response before the host was attached; dropped");
            }
        }
    }

    /// Gives a background response an ack token.
    fn with_ack(
        &self,
        mut response: NotificationResponse,
        background: bool,
    ) -> NotificationResponse {
        if background {
            let mut state = self.lock();
            let value = state.next_ack;
            state.next_ack += 1;
            state.acks.insert(value, response.id.clone());
            response.ack = Some(AckToken { value });
        }
        response
    }

    /// Hides `notification` and shows it again after `ms`.
    fn snooze(self: &Arc<Self>, notification: Notification, ms: u64) {
        let id = notification.id.clone();
        if let Err(err) = self.remove_ids(std::slice::from_ref(&id)) {
            tracing::debug!(%err, "notifications: snoozed notification not removed");
        }
        {
            let now = self.now();
            let mut state = self.lock();
            state.engine.add(notification, Trigger::After(ms), now);
            Self::changed_engine(&mut state);
        }
        self.persist();
        self.emit(NotificationEvent::Snoozed(id));
        self.rearm();
    }

    fn dismissed(&self, id: &NotificationId, reason: DismissReason) {
        let released = {
            let mut state = self.lock();
            let Some(live) = state.table.get_mut(id) else {
                return;
            };
            if live.dismissed {
                return;
            }
            live.dismissed = true;
            live.images_stale = true;
            live.expires_ms = None;
            let images = std::mem::take(&mut live.images);
            state.coalescer.dismissed(id);
            state.dismissed.push_back(id.clone());
            while state.dismissed.len() > MAX_DISMISSED {
                if let Some(old) = state.dismissed.pop_front() {
                    state.table.remove(&old);
                    state.coalescer.cancel(&old);
                }
            }
            images
        };
        lock(&self.images).release(&released);
        self.emit(NotificationEvent::Dismissed(Dismissal {
            id: id.clone(),
            reason,
        }));
    }

    fn failed(&self, id: Option<&NotificationId>, message: &str) {
        self.emit(NotificationEvent::Failed(Failure {
            id: id.cloned(),
            message: message.to_owned(),
        }));
    }

    fn backend_changed(self: &Arc<Self>) {
        let Ok(backend) = self.backend() else {
            return;
        };
        let rate = backend.info().rate;
        let now = self.now();
        let mut state = self.lock();
        if state.coalescer.rate() != rate && state.coalescer.queued() == 0 {
            state.coalescer = Coalescer::new(rate, now);
        }
    }
}

impl Tick for Inner {
    fn tick(self: &Arc<Self>) -> Option<i64> {
        self.run_due()
    }

    fn now_ms(&self) -> i64 {
        self.now()
    }
}

impl Notifications for DesktopNotifications {
    async fn capabilities(&self, ctx: HostCtx) -> Capabilities {
        self.inner.bind_ctx(&ctx);
        match self.inner.backend() {
            Ok(backend) => overlay(&backend.info()),
            Err(err) => {
                tracing::debug!(%err, "notifications: no desktop backend");
                Capabilities::none("unavailable")
            }
        }
    }

    async fn permission(&self, ctx: HostCtx) -> PermissionStatus {
        self.inner.bind_ctx(&ctx);
        self.inner.backend().map_or_else(
            |_| PermissionStatus::of(PermissionState::Unavailable),
            |backend| backend.permission(),
        )
    }

    async fn request_permission(
        &self,
        ctx: HostCtx,
        request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.backend().map_or_else(
            |_| Ok(PermissionStatus::of(PermissionState::Unavailable)),
            |backend| backend.request_permission(&request),
        )
    }

    async fn open_settings(
        &self,
        ctx: HostCtx,
        channel: Option<ChannelId>,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.backend()?.open_settings(channel.as_ref())
    }

    async fn set_channels(
        &self,
        ctx: HostCtx,
        channels: Vec<ChannelSpec>,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        for (i, channel) in channels.iter().enumerate() {
            channel.validate().map_err(|err| {
                InvalidField::new(format!("channels[{i}].{}", err.field), err.problem)
            })?;
        }
        self.inner.backend()?.set_channels(&channels)?;
        let mut state = self.inner.lock();
        for channel in channels {
            // Existing channels keep their settings except name and
            // description (the Android rule, applied everywhere).
            match state.channels.get_mut(&channel.id) {
                Some(existing) => {
                    existing.name = channel.name;
                    existing.description = channel.description;
                }
                None => {
                    state.channels.insert(channel.id.clone(), channel);
                }
            }
        }
        Ok(())
    }

    async fn delete_channel(&self, ctx: HostCtx, id: ChannelId) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.lock().channels.remove(&id);
        Ok(())
    }

    async fn show(
        &self,
        ctx: HostCtx,
        notification: Notification,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.show_now(notification, false)
    }

    async fn schedule(
        &self,
        ctx: HostCtx,
        notification: Notification,
        trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError> {
        self.inner.bind_ctx(&ctx);
        notification.validate()?;
        trigger.validate()?;
        let now = self.inner.now();
        let info = {
            let mut state = self.inner.lock();
            if let Some(channel) = &notification.channel
                && !state.channels.contains_key(channel)
            {
                return Err(NotificationError::UnknownChannel(channel.value.clone()));
            }
            let dead = trigger.never_fires();
            let info = state.engine.add(notification, trigger, now);
            if info.next_fire_ms.is_none() {
                return Err(dead.into());
            }
            Inner::changed_engine(&mut state);
            info
        };
        self.inner.persist();
        self.inner.rearm();
        Ok(info)
    }

    async fn update(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        patch: NotificationPatch,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        patch.validate()?;
        let urgency = if patch.alert_again {
            Urgency::Alert
        } else {
            Urgency::Regular
        };
        let touches_images = patch.icon.is_some()
            || patch.style.is_some()
            || patch.actions.is_some()
            || patch
                .clear
                .iter()
                .any(|field| matches!(field, PatchField::Icon | PatchField::Actions));
        self.inner.change(&id, urgency, |live| {
            live.notification.apply(patch);
            live.images_stale |= touches_images;
            Ok(())
        })
    }

    async fn set_progress(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        progress: Option<Progress>,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.change(&id, Urgency::Regular, |live| {
            live.notification.progress = progress;
            Ok(())
        })
    }

    async fn append_message(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        message: ChatMessage,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        message.validate()?;
        self.inner.change(&id, Urgency::Alert, |live| {
            let Style::Messaging(conversation) = &mut live.notification.style else {
                return Err(InvalidField::new("style", Problem::NotAllowedHere).into());
            };
            live.images_stale |= message.image.is_some()
                || message
                    .sender
                    .as_ref()
                    .is_some_and(|sender| sender.avatar.is_some());
            conversation.messages.push(message);
            Ok(())
        })
    }

    async fn cancel(&self, ctx: HostCtx, id: NotificationId) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.remove_ids(std::slice::from_ref(&id))
    }

    async fn cancel_group(&self, ctx: HostCtx, group: GroupId) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        let ids: Vec<NotificationId> = {
            let state = self.inner.lock();
            let in_group = |n: &Notification| n.group.as_ref().is_some_and(|g| g.id == group);
            state
                .table
                .values()
                .map(|live| &live.notification)
                .chain(
                    state
                        .engine
                        .entries()
                        .into_iter()
                        .map(|entry| &entry.notification),
                )
                .filter(|n| in_group(n))
                .map(|n| n.id.clone())
                .collect()
        };
        self.inner.remove_ids(&ids)
    }

    async fn cancel_all(&self, ctx: HostCtx) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        let ids: Vec<NotificationId> = {
            let state = self.inner.lock();
            state
                .table
                .keys()
                .cloned()
                .chain(
                    state
                        .engine
                        .entries()
                        .into_iter()
                        .map(|entry| entry.notification.id.clone()),
                )
                .collect()
        };
        self.inner.remove_ids(&ids)
    }

    async fn pending(&self, ctx: HostCtx) -> Result<Vec<PendingNotification>, NotificationError> {
        self.inner.bind_ctx(&ctx);
        Ok(self.inner.lock().engine.pending())
    }

    async fn delivered(
        &self,
        ctx: HostCtx,
    ) -> Result<Vec<DeliveredNotification>, NotificationError> {
        self.inner.bind_ctx(&ctx);
        let backend = self.inner.backend()?;
        if backend.info().lists_delivered {
            return backend.delivered();
        }
        let mut out: Vec<DeliveredNotification> = self
            .inner
            .lock()
            .table
            .values()
            .filter(|live| !live.dismissed)
            .map(|live| DeliveredNotification {
                id: live.notification.id.clone(),
                title: Some(live.notification.title.clone()),
                body: live.notification.body.clone(),
                group: live.notification.group.as_ref().map(|g| g.id.clone()),
                delivered_ms: Some(live.delivered_ms),
            })
            .collect();
        out.sort_by(|a, b| (a.delivered_ms, &a.id).cmp(&(b.delivered_ms, &b.id)));
        Ok(out)
    }

    async fn complete(&self, ctx: HostCtx, ack: AckToken) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        if self.inner.lock().acks.remove(&ack.value).is_none() {
            return Err(NotificationError::NotFound(format!("ack {}", ack.value)));
        }
        if let Ok(backend) = self.inner.backend() {
            backend.complete(ack);
        }
        Ok(())
    }

    fn events(&self) -> flume::Receiver<NotificationEvent> {
        lock(&self.inner.events).subscribe()
    }
}
