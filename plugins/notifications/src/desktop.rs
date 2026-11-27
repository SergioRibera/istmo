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
//! never takes the image cache (which writes and removes files) while it
//! holds the state lock: a post reusing the shown files shares them
//! through an `Arc`, so the state lock never waits on file IO.
//! Backend calls for one id go through that id's lane, one at a time in
//! the order they were issued (a newer post never lands before an older
//! one, a cancel lands after the post it follows); calls for different
//! ids run in parallel. What a backend reports through its sink is
//! handled on the report worker thread.

#![expect(
    clippy::significant_drop_tightening,
    reason = "state guards live in short blocks that end right after their last use; the lint asks for explicit drops there"
)]

#[cfg(any(target_os = "macos", test))]
mod apple_map;
mod backend;
mod images;
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "UserNotifications and Intents through objc2: the delegate class, user info, attachments, intents"
)]
mod macos;
mod timer;
#[cfg(any(windows, test))]
pub mod windows;

#[cfg(test)]
mod tests;

use std::collections::hash_map::{Entry as Slot, RandomState};
use std::collections::{HashMap, HashSet, VecDeque};
use std::hash::BuildHasher as _;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock, PoisonError, Weak};

use istmo_core::sync::lock;
use istmo_core::{HostCtx, Runtime};
use jiff::tz::TimeZone;

pub use backend::{
    ARM_CAP, ARM_WINDOW_MS, Activation, Backend, BackendInfo, BackendSink, Carried, ImageFiles,
    ImageSlot, NullBackend, ShowMode, Shown, Upcoming,
};

use crate::coalesce::{Admit, Coalescer, Rate, Urgency};
use crate::render::{Labels, RenderLimits, Resolved, render};
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
use timer::{HostTimer, ManualTimer, Moved, Serve, Tick, Timer, Worker};

/// Events kept for the first subscriber.
const BACKLOG: usize = 64;
/// Depth of each subscriber's channel.
const SUBSCRIBER_DEPTH: usize = 256;
/// Dismissed notifications kept (for an alerting update to show them
/// again); older ones are forgotten.
const MAX_DISMISSED: usize = 256;
/// Ack tokens of background responses kept; the oldest unanswered one
/// is forgotten past this.
const MAX_ACKS: usize = 256;

/// Starts the platform backend, handing it the sink it reports through.
pub type StartBackend =
    Box<dyn Fn(BackendSink) -> Result<Arc<dyn Backend>, NotificationError> + Send + Sync>;

/// The platform backend of this target. Platform modules add their arm;
/// targets without one answer like `UnavailableNotifications`.
#[cfg(target_os = "linux")]
fn platform_backend(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    linux::platform_backend(sink)
}

/// The platform backend of this target. Platform modules add their arm;
/// targets without one answer like `UnavailableNotifications`.
/// macOS: `UNUserNotificationCenter` (needs an app bundle).
#[cfg(target_os = "macos")]
fn platform_backend(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    macos::start(sink)
}

/// The platform backend of this target. Platform modules add their arm;
/// targets without one answer like `UnavailableNotifications`.
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn platform_backend(_sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    Err(NotificationError::Unsupported(Feature::Show))
}

/// Windows: toasts through the registration made by
/// [`DesktopNotifications::install`].
#[cfg(windows)]
fn platform_backend(sink: BackendSink) -> Result<Arc<dyn Backend>, NotificationError> {
    windows::start(sink)
}

/// A shown notification as the host keeps it.
#[derive(Debug)]
struct Live {
    /// Latest content (queued updates included). Shared with the post in
    /// flight, so a progress update copies nothing.
    notification: Arc<Notification>,
    /// What the backend last got.
    rendered: crate::render::Rendered,
    /// Shared with posts in flight: each `Arc` is one use of the cached
    /// files, released by whoever drops the last clone
    /// ([`Inner::release`]).
    images: Arc<ImageFiles>,
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
    /// Ack tokens of background responses, oldest first.
    acks: VecDeque<(u64, NotificationId)>,
    /// Keys the ack tokens: tokens are a keyed hash of a counter, not
    /// guessable from one another.
    ack_keys: RandomState,
    ack_counter: u64,
    /// Ids with a backend call in flight (their lane is taken).
    lanes: HashSet<NotificationId>,
    /// Callers waiting for a lane: a release wakes them only when
    /// there are any (no futex call on the uncontended path).
    lane_waiters: usize,
    /// Scheduled deliveries taken from the engine and not yet shown, by
    /// the ticket they were taken with; a cancel removes them.
    delivering: HashMap<NotificationId, u64>,
    next_ticket: u64,
    /// Store loaded and missed deliveries applied.
    loaded: bool,
    /// Bumped on every engine change; the store keeps the newest.
    store_generation: u64,
    /// When an entry past the [`ARM_WINDOW_MS`] window of the last
    /// [`Backend::arm`] enters it: the window rolls forward then, also
    /// while nothing else changes.
    arm_at: Option<i64>,
}

impl State {
    /// A fresh ack token for `id`.
    fn new_ack(&mut self, id: NotificationId) -> u64 {
        loop {
            self.ack_counter += 1;
            let value = self.ack_keys.hash_one(self.ack_counter);
            if value != 0 && self.acks.iter().all(|(token, _)| *token != value) {
                if self.acks.len() == MAX_ACKS {
                    self.acks.pop_front();
                }
                self.acks.push_back((value, id));
                return value;
            }
        }
    }
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

/// What the host derives from [`Backend::info`], read once at start and
/// again after [`BackendSink::changed`].
#[derive(Debug)]
struct HostInfo {
    info: BackendInfo,
    /// The backend's limits with the host's labels.
    limits: RenderLimits,
    /// The wire report: the backend's with the host's simulations.
    capabilities: Capabilities,
}

impl HostInfo {
    fn new(info: BackendInfo, labels: &Labels) -> Self {
        let mut limits = info.limits.clone();
        limits.labels.clone_from(labels);
        Self {
            limits,
            capabilities: overlay(&info),
            info,
        }
    }
}

/// The cached [`HostInfo`].
#[derive(Debug, Default)]
struct InfoSlot {
    host: Option<Arc<HostInfo>>,
    /// Bumped when the backend reports a change: a read that started
    /// before it does not replace the newer one.
    version: u64,
}

/// A backend report, handled on the report worker.
enum Report {
    Activated(Activation),
    Dismissed(NotificationId, DismissReason),
    Failed(Option<NotificationId>, String),
    Changed,
    /// Answered once everything queued before it was handled.
    Settle(flume::Sender<()>),
}

/// The shared host behind every clone of [`DesktopNotifications`].
pub(crate) struct Inner {
    start: StartBackend,
    backend: OnceLock<Result<Arc<dyn Backend>, NotificationError>>,
    info: Mutex<InfoSlot>,
    runtime: OnceLock<Weak<Runtime>>,
    /// Set once the store is loaded; every call waits for it.
    loaded: OnceLock<()>,
    state: Mutex<State>,
    /// Signalled when a lane is released.
    lane_free: Condvar,
    events: Mutex<EventHub>,
    images: Mutex<ImageCache>,
    store: Option<Store>,
    /// Serialises store writes; holds the generation last written.
    store_written: Mutex<u64>,
    /// Serialises `Backend::arm` calls (see `arm_native`).
    arming: Mutex<()>,
    timer: OnceLock<HostTimer<Self>>,
    reports: Worker<Report>,
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
/// (`hosts: [Notifications => DesktopNotifications::default()]`).
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
        Self::platform().build()
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
            info: Mutex::default(),
            runtime: OnceLock::new(),
            loaded: OnceLock::new(),
            state: Mutex::new(State {
                table: HashMap::new(),
                dismissed: VecDeque::new(),
                // Replaced by the backend's rate when its info is first read.
                coalescer: Coalescer::new(Rate::LINUX, now),
                engine: Engine::default(),
                channels,
                acks: VecDeque::new(),
                ack_keys: RandomState::new(),
                ack_counter: 0,
                lanes: HashSet::new(),
                lane_waiters: 0,
                delivering: HashMap::new(),
                next_ticket: 0,
                loaded: false,
                store_generation: 0,
                arm_at: None,
            }),
            lane_free: Condvar::new(),
            events: Mutex::default(),
            images: Mutex::new(ImageCache::new(self.images)),
            store: self.store.map(Store::new),
            store_written: Mutex::new(0),
            arming: Mutex::new(()),
            timer: OnceLock::new(),
            reports: Worker::default(),
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
    /// What [`Default`] builds, to configure first: usually
    /// `.with_channels(istmo_notifications::static_channels!())`.
    #[must_use]
    pub fn platform() -> DesktopNotificationsBuilder {
        Self::builder(Box::new(platform_backend))
    }

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

    /// Waits until every backend report made before the call (through a
    /// [`BackendSink`]) is handled. Reports are handled on the host's
    /// report worker, never on the reporting thread; tests and backends
    /// that need the effect of a report call this. Returns at once on
    /// the report worker itself.
    pub fn settle(&self) {
        if timer::on_worker() {
            return;
        }
        let (tx, rx) = flume::bounded(1);
        self.inner.report(Report::Settle(tx));
        let _settled = rx.recv();
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

/// One post to make, prepared in the id's lane.
struct Post {
    notification: Arc<Notification>,
    mode: ShowMode,
    /// Collect the images again (a full show); otherwise the shown
    /// ones are reused unless an update touched them.
    fresh: bool,
    /// Emitted once the post went out.
    event: Option<NotificationEvent>,
}

/// The lane of one id, held while its backend call runs. Dropping it
/// (also on unwind) frees the lane.
struct Lane<'a> {
    inner: &'a Inner,
    id: NotificationId,
    /// The state lock, when held.
    guard: Option<MutexGuard<'a, State>>,
}

impl Lane<'_> {
    /// The state, locking it if needed.
    fn state(&mut self) -> &mut State {
        let inner = self.inner;
        self.guard.get_or_insert_with(|| inner.lock())
    }

    /// Lets go of the state lock (before a backend call).
    fn unlock(&mut self) {
        self.guard = None;
    }
}

impl Drop for Lane<'_> {
    fn drop(&mut self) {
        let mut state = self.guard.take().unwrap_or_else(|| self.inner.lock());
        state.lanes.remove(&self.id);
        let waiting = state.lane_waiters > 0;
        drop(state);
        if waiting {
            self.inner.lane_free.notify_all();
        }
    }
}

impl Inner {
    fn now(&self) -> i64 {
        (self.clock)()
    }

    fn lock(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Takes the lane of `id`, waiting while another call holds it. The
    /// state stays locked from the wait on, so the caller sees what the
    /// previous call left.
    fn acquire(&self, id: &NotificationId) -> Lane<'_> {
        let mut state = self.lock();
        while state.lanes.contains(id) {
            state.lane_waiters += 1;
            state = self
                .lane_free
                .wait(state)
                .unwrap_or_else(PoisonError::into_inner);
            state.lane_waiters -= 1;
        }
        state.lanes.insert(id.clone());
        Lane {
            inner: self,
            id: id.clone(),
            guard: Some(state),
        }
    }

    fn backend(self: &Arc<Self>) -> Result<&Arc<dyn Backend>, NotificationError> {
        // The start function's sink calls only queue reports, so they
        // never re-enter this initialisation.
        self.backend
            .get_or_init(|| {
                (self.start)(BackendSink {
                    host: Arc::downgrade(self),
                })
            })
            .as_ref()
            .map_err(Clone::clone)
    }

    /// The backend and what the host derived from its info.
    fn backend_info(
        self: &Arc<Self>,
    ) -> Result<(Arc<dyn Backend>, Arc<HostInfo>), NotificationError> {
        let backend = Arc::clone(self.backend()?);
        let cached = {
            let slot = lock(&self.info);
            slot.host.clone().ok_or(slot.version)
        };
        let host = match cached {
            Ok(host) => host,
            Err(version) => self.read_info(backend.as_ref(), version),
        };
        Ok((backend, host))
    }

    /// Reads the backend's info (no lock held) and caches it unless a
    /// change was reported since `version`; applies its rate.
    fn read_info(&self, backend: &dyn Backend, version: u64) -> Arc<HostInfo> {
        let host = Arc::new(HostInfo::new(backend.info(), &self.labels));
        {
            let mut slot = lock(&self.info);
            if slot.version != version {
                return host;
            }
            slot.host = Some(Arc::clone(&host));
        }
        let now = self.now();
        self.lock().coalescer.set_rate(host.info.rate, now);
        host
    }

    fn emit(&self, event: NotificationEvent) {
        lock(&self.events).emit(event);
    }

    fn report(self: &Arc<Self>, report: Report) {
        self.reports.send(self, report);
    }

    fn runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime.get().and_then(Weak::upgrade)
    }

    /// Binds the runtime of the first call; waits for the load.
    fn bind_ctx(self: &Arc<Self>, ctx: &HostCtx) {
        if self.runtime.get().is_none()
            && let Ok(runtime) = ctx.runtime()
        {
            let _set = self.runtime.set(Arc::downgrade(&runtime));
        }
        self.ensure_loaded();
    }

    fn bind(self: &Arc<Self>, runtime: &Arc<Runtime>) {
        let _set = self.runtime.set(Arc::downgrade(runtime));
        self.ensure_loaded();
    }

    /// Loads once; every other caller waits until the load is done, so
    /// no call sees the engine before the stored entries are in it.
    fn ensure_loaded(self: &Arc<Self>) {
        self.loaded.get_or_init(|| self.load());
    }

    /// Loads the store, sweeps stale images and delivers what was missed.
    fn load(self: &Arc<Self>) {
        let stored = match self.store.as_ref().map(Store::load) {
            Some(Ok(entries)) => entries,
            Some(Err(err)) => {
                tracing::warn!(%err, "notifications: schedule store unreadable; starting empty");
                Vec::new()
            }
            None => Vec::new(),
        };
        lock(&self.images).sweep(std::time::SystemTime::now());
        // Starts the backend and takes its rate.
        if let Err(err) = self.backend_info() {
            tracing::debug!(%err, "notifications: no desktop backend");
        }
        let local = TimeZone::system();
        {
            let mut state = self.lock();
            let mut engine = Engine::with_entries(local, stored);
            // Entries scheduled before the load (a snooze reported
            // early) replace stored ones with the same id.
            let early = std::mem::replace(&mut state.engine, Engine::new(TimeZone::UTC));
            if !early.is_empty() {
                Self::changed_engine(&mut state);
            }
            engine.merge(early);
            state.engine = engine;
            state.loaded = true;
        }
        // `run_due` persists (and arms) what it changes; the arm below
        // covers a load that changed nothing.
        self.write_store();
        self.run_due();
        self.arm_native();
    }

    /// Hands the next deliveries to the backend ([`Backend::arm`]). Only
    /// the store's owner arms: a forwarding process has an empty schedule
    /// and would disarm the owner's. Only the snapshot is taken under the
    /// state lock; rendering and the backend call run outside it, one arm
    /// at a time so a later snapshot is never overtaken by an older one.
    fn arm_native(self: &Arc<Self>) {
        if self.store.is_none() {
            return;
        }
        let Ok((backend, host)) = self.backend_info() else {
            return;
        };
        let _one_at_a_time = lock(&self.arming);
        let (entries, channels) = {
            let state = self.lock();
            if !state.loaded {
                return;
            }
            (state.engine.snapshot(), state.channels.clone())
        };
        let now = self.now();
        let horizon = now.saturating_add(ARM_WINDOW_MS);
        let mut upcoming = Vec::new();
        // Occurrences past the cap are re-armed by the deliveries before
        // them (each one changes the engine); only the window edge needs
        // a wake of its own.
        let mut edge: Option<i64> = None;
        for entry in &entries {
            let mut notification = entry.notification.clone();
            if notification.validate().is_err()
                || merge_channel(&mut notification, &channels).is_err()
            {
                continue;
            }
            notification.normalize();
            let rendered = render(&notification, &host.limits);
            let mut next = entry.next_fire_ms;
            let mut count = 0;
            while let Some(fire_ms) = next.filter(|&at| at <= horizon && count < ARM_CAP) {
                if fire_ms > now {
                    upcoming.push(Upcoming {
                        notification: notification.clone(),
                        rendered: rendered.clone(),
                        fire_ms,
                    });
                    count += 1;
                }
                next = entry.trigger.next_after(entry.anchor_ms, fire_ms);
            }
            if let Some(at) = next.filter(|&at| at > horizon) {
                let enters = at.saturating_sub(ARM_WINDOW_MS);
                edge = Some(edge.map_or(enters, |edge| edge.min(enters)));
            }
        }
        upcoming
            .sort_by(|a, b| (a.fire_ms, &a.notification.id).cmp(&(b.fire_ms, &b.notification.id)));
        upcoming.truncate(ARM_CAP);
        self.lock().arm_at = edge;
        backend.arm(&upcoming);
    }

    /// Writes the engine's entries if they changed since the last write,
    /// then re-arms the backend's own deliveries.
    fn persist(self: &Arc<Self>) {
        if self.write_store() {
            self.arm_native();
        }
    }

    /// Writes the engine's entries if they changed; `true` when it tried.
    fn write_store(&self) -> bool {
        let Some(store) = &self.store else {
            return false;
        };
        let (generation, mut entries) = {
            let state = self.lock();
            // Before the load the file still holds what must be read.
            if !state.loaded {
                return false;
            }
            (state.store_generation, state.engine.snapshot())
        };
        let mut written = lock(&self.store_written);
        if *written >= generation {
            return false;
        }
        let bytes = if entries.is_empty() {
            Ok(None)
        } else {
            entries.sort_by(|a, b| a.order().cmp(&b.order()));
            Store::encode(entries.iter().map(AsRef::as_ref)).map(Some)
        };
        match bytes.and_then(|bytes| store.write(bytes.as_deref())) {
            Ok(()) => *written = generation,
            Err(err) => tracing::warn!(%err, "notifications: schedule store not written"),
        }
        true
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
            state.arm_at,
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

    /// Re-reads the device's zone; when it changed, floating calendar
    /// rules move to it. The new deadline when anything moved.
    fn zone_check(self: &Arc<Self>) -> Option<Moved> {
        // Cached by jiff (re-read every few minutes): cheap, and never
        // under the state lock.
        let local = TimeZone::system();
        let now = self.now();
        let deadline = {
            let mut state = self.lock();
            if !state.loaded || !state.engine.set_local(local, now) {
                return None;
            }
            Self::changed_engine(&mut state);
            Self::next_deadline(&state)
        };
        self.persist();
        Some(Moved(deadline))
    }

    /// Sends `post` through the backend from inside its lane: images are
    /// collected and the body rendered with the state unlocked, the
    /// result recorded under it.
    fn post(
        &self,
        lane: &mut Lane<'_>,
        backend: &dyn Backend,
        host: &HostInfo,
        post: Post,
    ) -> Result<(), NotificationError> {
        let Post {
            notification,
            mode,
            fresh,
            event,
        } = post;
        // The clone is this post's use: the shown record cannot release
        // the files while the post holds it.
        let reused = lane
            .state()
            .table
            .get(&notification.id)
            .filter(|live| !fresh && !live.dismissed && !live.images_stale)
            .map(|live| Arc::clone(&live.images));
        lane.unlock();
        let images = reused.unwrap_or_else(|| Arc::new(lock(&self.images).collect(&notification)));
        let rendered = render(&notification, &host.limits);
        let shown = backend.show(&Shown {
            notification: &notification,
            rendered: &rendered,
            images: &images,
            mode,
        });
        if let Err(err) = shown {
            self.release(images);
            return Err(err);
        }
        let now = self.now();
        let expires_ms = if host.info.timeouts {
            None
        } else {
            notification
                .timeout_ms
                .map(|ms| now.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)))
        };
        let replaced = {
            let state = lane.state();
            forget_dismissed(state, &notification.id);
            match state.table.entry(notification.id.clone()) {
                Slot::Occupied(mut slot) => {
                    let live = slot.get_mut();
                    if live.dismissed || mode == ShowMode::New {
                        live.delivered_ms = now;
                    }
                    live.notification = notification;
                    live.rendered = rendered;
                    live.images_stale = false;
                    live.dismissed = false;
                    live.expires_ms = expires_ms;
                    std::mem::replace(&mut live.images, images)
                }
                Slot::Vacant(slot) => {
                    slot.insert(Live {
                        notification,
                        rendered,
                        images,
                        images_stale: false,
                        delivered_ms: now,
                        dismissed: false,
                        expires_ms,
                    });
                    Arc::default()
                }
            }
        };
        lane.unlock();
        self.release(replaced);
        if let Some(event) = event {
            self.emit(event);
        }
        Ok(())
    }

    /// Drops one use of `files`; the last one releases the cached files.
    /// A replaced record that shares them with its successor releases
    /// nothing and takes no lock.
    fn release(&self, files: Arc<ImageFiles>) {
        if let Some(files) = Arc::into_inner(files)
            && !files.is_empty()
        {
            lock(&self.images).release(&files);
        }
    }

    /// A full show: replaces whatever has this id. `ticket` is the
    /// engine ticket of a scheduled delivery: a cancel since it was
    /// taken skips the show.
    fn show_now(
        self: &Arc<Self>,
        mut notification: Notification,
        overdue: bool,
        ticket: Option<u64>,
    ) -> Result<(), NotificationError> {
        notification.validate()?;
        notification.normalize();
        let (backend, host) = self.backend_info()?;
        let now = self.now();
        let mut lane = self.acquire(&notification.id);
        let mode = {
            let state = lane.state();
            if let Some(ticket) = ticket {
                if state.delivering.get(&notification.id) != Some(&ticket) {
                    return Ok(());
                }
                state.delivering.remove(&notification.id);
            }
            merge_channel(&mut notification, &state.channels)?;
            let shown = state
                .table
                .get(&notification.id)
                .is_some_and(|live| !live.dismissed);
            state.coalescer.shown(&notification.id, now);
            if shown {
                ShowMode::Replace { alert: true }
            } else {
                ShowMode::New
            }
        };
        let id = notification.id.clone();
        let event = if overdue {
            NotificationEvent::DeliveredLate(id)
        } else {
            NotificationEvent::Delivered(id)
        };
        let post = Post {
            notification: Arc::new(notification),
            mode,
            fresh: true,
            event: Some(event),
        };
        self.post(&mut lane, backend.as_ref(), &host, post)?;
        drop(lane);
        self.rearm();
        Ok(())
    }

    /// Changes the shown `id` with `edit` (which returns whether it
    /// touched the images); posts through the coalescer. `edit` checks
    /// what it changes: the host does not validate the whole
    /// notification again.
    fn change(
        self: &Arc<Self>,
        id: &NotificationId,
        urgency: Urgency,
        edit: impl FnOnce(&mut Notification) -> Result<bool, NotificationError>,
    ) -> Result<(), NotificationError> {
        let (backend, host) = self.backend_info()?;
        let now = self.now();
        let mut lane = self.acquire(id);
        let post = {
            let state = lane.state();
            let live = state
                .table
                .get_mut(id)
                .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
            // Copies only when someone else still holds the content.
            let touched = edit(Arc::make_mut(&mut live.notification))?;
            live.images_stale |= touched;
            let was_dismissed = live.dismissed;
            let content = Arc::clone(&live.notification);
            match state.coalescer.offer(id, (), urgency, now) {
                Admit::Dropped | Admit::Queued => None,
                Admit::Now(()) => Some(Post {
                    notification: content,
                    mode: if was_dismissed {
                        ShowMode::New
                    } else {
                        ShowMode::Replace {
                            alert: urgency != Urgency::Regular,
                        }
                    },
                    fresh: false,
                    event: None,
                }),
            }
        };
        if let Some(post) = post {
            self.post(&mut lane, backend.as_ref(), &host, post)?;
        }
        drop(lane);
        self.rearm();
        Ok(())
    }

    /// Takes `ids` off the screen and out of the schedule. Each id waits
    /// for the backend call in flight on it, so the removal lands last.
    fn remove_ids(self: &Arc<Self>, ids: &[NotificationId]) -> Result<(), NotificationError> {
        {
            let mut state = self.lock();
            let mut engine_changed = false;
            for id in ids {
                engine_changed |= state.engine.remove(id);
                // A scheduled delivery taken but not shown yet is off.
                state.delivering.remove(id);
                state.coalescer.cancel(id);
            }
            if engine_changed {
                Self::changed_engine(&mut state);
            }
        }
        self.persist();
        let mut first_error = None;
        for id in ids {
            let mut lane = self.acquire(id);
            let removed = {
                let state = lane.state();
                state.coalescer.cancel(id);
                forget_dismissed(state, id);
                state.acks.retain(|(_, acked)| acked != id);
                state.table.remove(id)
            };
            lane.unlock();
            // An id the table does not know may still be on screen, shown
            // by an earlier process (a toast, a portal notification): the
            // backend decides whether it can find it.
            let known = match removed {
                Some(live) => {
                    let dismissed = live.dismissed;
                    self.release(live.images);
                    if dismissed {
                        continue;
                    }
                    true
                }
                None => false,
            };
            let backend = match self.backend() {
                Ok(backend) => backend,
                Err(err) if known => return Err(err),
                Err(_) => continue,
            };
            if let Err(err) = backend.remove(id) {
                first_error.get_or_insert(err);
            }
        }
        self.rearm();
        first_error.map_or(Ok(()), Err)
    }

    /// Runs what is due: scheduled deliveries, queued updates, timeouts.
    fn run_due(self: &Arc<Self>) -> Option<i64> {
        self.zone_check();
        let now = self.now();
        let (due, queued, expired, roll) = {
            let mut state = self.lock();
            if !state.loaded {
                return None;
            }
            let due: Vec<_> = state
                .engine
                .due(now)
                .into_iter()
                .map(|due| {
                    state.next_ticket += 1;
                    let ticket = state.next_ticket;
                    state.delivering.insert(due.notification.id.clone(), ticket);
                    (due, ticket)
                })
                .collect();
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
            // The arm window's edge passed: arm again (a delivery below
            // arms through `persist` anyway).
            let roll = state.arm_at.is_some_and(|at| at <= now);
            if roll {
                state.arm_at = None;
            }
            (due, queued, expired, roll)
        };
        if !due.is_empty() {
            self.persist();
        } else if roll {
            self.arm_native();
        }

        for (entry, ticket) in due {
            let id = entry.notification.id.clone();
            if let Err(err) = self.show_now(entry.notification, entry.late, Some(ticket)) {
                let mut state = self.lock();
                if state.delivering.get(&id) == Some(&ticket) {
                    state.delivering.remove(&id);
                }
                drop(state);
                self.failed(Some(&id), &err.to_string());
            }
        }
        if !queued.is_empty()
            && let Ok((backend, host)) = self.backend_info()
        {
            for id in queued {
                let mut lane = self.acquire(&id);
                let content = lane
                    .state()
                    .table
                    .get(&id)
                    .filter(|live| !live.dismissed)
                    .map(|live| Arc::clone(&live.notification));
                let Some(notification) = content else {
                    continue;
                };
                let post = Post {
                    notification,
                    mode: ShowMode::Replace { alert: false },
                    fresh: false,
                    event: None,
                };
                if let Err(err) = self.post(&mut lane, backend.as_ref(), &host, post) {
                    drop(lane);
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
                (resolved, background, Arc::clone(&live.notification))
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
            let value = self.lock().new_ack(response.id.clone());
            response.ack = Some(AckToken { value });
        }
        response
    }

    /// Hides `notification` and shows it again after `ms`.
    fn snooze(self: &Arc<Self>, notification: Arc<Notification>, ms: u64) {
        let id = notification.id.clone();
        if let Err(err) = self.remove_ids(std::slice::from_ref(&id)) {
            tracing::debug!(%err, "notifications: snoozed notification not removed");
        }
        // The table let go of it: no copy unless someone else holds it.
        let notification = Arc::unwrap_or_clone(notification);
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
                    // A post in flight on `old` records it again.
                    state.table.remove(&old);
                    state.coalescer.cancel(&old);
                }
            }
            images
        };
        self.release(released);
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

    /// The backend reported a change: its info is read again and its
    /// rate applies at once (queued updates stay queued).
    fn backend_changed(&self) {
        // `get`: a change reported while the backend starts is covered
        // by the read that follows the start.
        let Some(Ok(backend)) = self.backend.get() else {
            return;
        };
        let version = {
            let mut slot = lock(&self.info);
            slot.version += 1;
            slot.host = None;
            slot.version
        };
        self.read_info(backend.as_ref(), version);
        self.rearm();
    }
}

impl Serve<Report> for Inner {
    fn serve(self: &Arc<Self>, report: Report) {
        match report {
            Report::Activated(activation) => self.activated(activation),
            Report::Dismissed(id, reason) => self.dismissed(&id, reason),
            Report::Failed(id, message) => self.failed(id.as_ref(), &message),
            Report::Changed => self.backend_changed(),
            Report::Settle(done) => {
                let _answered = done.send(());
            }
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

    fn idle(self: &Arc<Self>) -> Option<Moved> {
        self.zone_check()
    }
}

impl Notifications for DesktopNotifications {
    async fn capabilities(&self, ctx: HostCtx) -> Capabilities {
        self.inner.bind_ctx(&ctx);
        match self.inner.backend_info() {
            Ok((_, host)) => host.capabilities.clone(),
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
        self.inner.show_now(notification, false, None)
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
        self.inner.change(&id, urgency, |notification| {
            let before = notification.clone();
            notification.apply(patch);
            // The whole notification: a patch alone cannot tell (call
            // actions need a call style, fields depend on each other).
            if let Err(err) = notification.validate() {
                *notification = before;
                return Err(err.into());
            }
            notification.normalize();
            Ok(touches_images)
        })
    }

    async fn set_progress(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        progress: Option<Progress>,
    ) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        if let Some(bar) = &progress {
            bar.validate()?;
        }
        // Only the bar changes: nothing else to check again.
        self.inner.change(&id, Urgency::Regular, |notification| {
            notification.progress = progress;
            Ok(false)
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
        self.inner.change(&id, Urgency::Alert, |notification| {
            let Style::Messaging(conversation) = &mut notification.style else {
                return Err(InvalidField::new("style", Problem::NotAllowedHere).into());
            };
            let touched = message.image.is_some()
                || message
                    .sender
                    .as_ref()
                    .is_some_and(|sender| sender.avatar.is_some());
            conversation.messages.push(message);
            notification.normalize();
            Ok(touched)
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
                .map(|live| &*live.notification)
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
        let removed = self.inner.remove_ids(&ids);
        // What earlier processes left on screen, where the platform can
        // find it.
        let rest = self
            .inner
            .backend()
            .map_or(Ok(()), |backend| backend.remove_all());
        removed.and(rest)
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
        let (backend, host) = self.inner.backend_info()?;
        if host.info.lists_delivered {
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
        let known = {
            let mut state = self.inner.lock();
            let at = state.acks.iter().position(|(token, _)| *token == ack.value);
            at.and_then(|at| state.acks.remove(at)).is_some()
        };
        if !known {
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
