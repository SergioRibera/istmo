//! [`WebNotifications`]: the [`State`] machine wired to the browser.
//!
//! Threads: none (`wasm32-unknown-unknown`). Rust state sits behind
//! mutexes only because the trait wants `Send + Sync`; JS objects sit in
//! one `SendWrapper<RefCell<..>>` and are touched only between awaits.
//! Browser calls that touch notifications go through one serial task
//! (`ops`), so a show, its updates and its close reach the browser in
//! the order the state decided them (`showNotification` resolves after
//! its images loaded, so two concurrent calls could land out of order).

#![allow(
    clippy::future_not_send,
    reason = "wasm32-unknown-unknown is single-threaded: these futures run on the JS event loop, behind `js::local` where a `Send` one is needed"
)]
#![expect(
    clippy::significant_drop_tightening,
    reason = "guards live in short blocks that end right after their last use; the lint asks for explicit drops there"
)]

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, Weak};

use istmo_core::sync::lock;
use istmo_core::{HostCtx, Runtime};
use jiff::tz::TimeZone;
use js_sys::{Array, Uint8Array};
use send_wrapper::SendWrapper;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::{JsCast, JsValue};

use super::caps::{Browser, UserAgent, refuses_page_notifications};
use super::images::{self, ImageKey, Source};
use super::lease::{Lease, Message, Step};
use super::payload::{self, DATA_VERSION, Options, Payload, Urls};
use super::state::{Activated, Post, State, decode_records};
use super::timer::Timeout;
use super::{idb, js};
use crate::coalesce::Urgency;
use crate::render::Labels;
use crate::schedule::Wakeup;
use crate::web_assets::{CHANNEL_NAME, LOCK_NAME, SERVICE_WORKER_FILE};
use crate::{
    AckToken, Capabilities, ChannelId, ChannelSpec, ChatMessage, DeliveredNotification,
    DismissReason, Feature, GroupId, InvalidField, Notification, NotificationError,
    NotificationEvent, NotificationId, NotificationPatch, NotificationResponses, Notifications,
    PatchField, PendingNotification, PermissionRequest, PermissionState, PermissionStatus,
    PlatformError, Problem, Progress, ScheduledInfo, Style, Trigger,
};

/// How long the first call waits for the service worker to become
/// active before it goes on without it (it is adopted when it arrives).
const READY_WAIT_MS: i32 = 10_000;
/// `data.istmo` messages of the worker.
const MESSAGE: &str = "istmo.notifications";

/// Which service worker shows the notifications.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ServiceWorker {
    /// Register the plugin's script at this URL (relative to the page).
    /// When the page's scope already has a registration for another
    /// script, that one is used instead (it must `importScripts` the
    /// plugin's) and nothing is replaced.
    Register(String),
    /// Use the registration the app made itself; its script must
    /// `importScripts("istmo-notifications-sw.js")`.
    Existing,
    /// No service worker: page notifications only (no buttons, no taps
    /// while the page is closed; nothing at all on Chrome for Android).
    None,
}

impl Default for ServiceWorker {
    /// [`ServiceWorker::Register`] of [`SERVICE_WORKER_FILE`].
    fn default() -> Self {
        Self::Register(SERVICE_WORKER_FILE.to_owned())
    }
}

/// Configures a [`WebNotifications`].
#[derive(Debug, Clone)]
pub struct WebNotificationsBuilder {
    service_worker: ServiceWorker,
    channels: Vec<ChannelSpec>,
    labels: Labels,
    open_url: Option<String>,
    store: bool,
}

impl WebNotificationsBuilder {
    /// Which service worker to use.
    #[must_use]
    pub fn service_worker(mut self, service_worker: ServiceWorker) -> Self {
        self.service_worker = service_worker;
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

    /// The page the worker opens for a tap when no window of the app is
    /// open. Default: the registration's scope.
    #[must_use]
    pub fn open_url(mut self, url: impl Into<String>) -> Self {
        self.open_url = Some(url.into());
        self
    }

    /// Keeps scheduled notifications in memory only (no `IndexedDB`). The
    /// worker's queue of taps needs `IndexedDB`, so taps while no page
    /// runs are lost too.
    #[must_use]
    pub const fn without_store(mut self) -> Self {
        self.store = false;
        self
    }

    /// The host.
    #[must_use]
    pub fn build(self) -> WebNotifications {
        let (ops, queue) = flume::unbounded();
        let inner = Arc::new_cyclic(|me: &Weak<Inner>| {
            let timer_host = me.clone();
            Inner {
                me: me.clone(),
                state: Mutex::new(State::new(
                    Browser::default().limits(&self.labels),
                    self.channels,
                    now(),
                )),
                browser: Mutex::new(Browser::default()),
                phase: Mutex::new(Phase::Idle),
                runtime: OnceLock::new(),
                lease: Mutex::new(None),
                reloads: Mutex::new(Reloads::default()),
                timer: Timeout::new(
                    move || {
                        if let Some(inner) = timer_host.upgrade() {
                            inner.timer.fired();
                            inner.run_due();
                        }
                    },
                    now,
                ),
                js: SendWrapper::new(RefCell::new(JsState::default())),
                ops,
                config: Config {
                    service_worker: self.service_worker,
                    labels: self.labels,
                    open_url: self.open_url,
                    store: self.store,
                },
            }
        });
        let worker = Arc::downgrade(&inner);
        wasm_bindgen_futures::spawn_local(async move {
            while let Ok(op) = queue.recv_async().await {
                let Some(inner) = worker.upgrade() else {
                    return;
                };
                inner.run_op(op).await;
            }
        });
        WebNotifications { inner }
    }
}

/// The notifications host for the web (`wasm32-unknown-unknown`).
///
/// Register it with the runtime
/// (`hosts: [Notifications => WebNotifications::default()]`) and
/// [`attach`](Self::attach) a clone at start, so taps queued by the
/// service worker and missed scheduled notifications are handled before
/// the app's first call.
///
/// Scheduling runs while the page is open ([`crate::Support::WhileRunning`]);
/// entries persist in `IndexedDB` and missed ones follow their
/// [`crate::MissedPolicy`] when the page loads again. Background tabs
/// throttle timers, so deliveries there can be late by up to a minute.
#[derive(Debug, Clone)]
pub struct WebNotifications {
    inner: Arc<Inner>,
}

impl Default for WebNotifications {
    /// The plugin's service worker at [`SERVICE_WORKER_FILE`], `IndexedDB`.
    fn default() -> Self {
        Self::builder().build()
    }
}

impl WebNotifications {
    /// A builder with the defaults.
    #[must_use]
    pub fn builder() -> WebNotificationsBuilder {
        WebNotificationsBuilder {
            service_worker: ServiceWorker::default(),
            channels: Vec::new(),
            labels: Labels::default(),
            open_url: None,
            store: true,
        }
    }

    /// Binds the host to `runtime` and starts loading: finds the service
    /// worker, reads the schedule, adopts notifications earlier pages
    /// left on screen and drains the worker's queue of taps. Without it
    /// this happens on the first call.
    pub fn attach(&self, runtime: &Arc<Runtime>) {
        self.inner.bind(runtime);
        let _started = self.inner.wait_ready();
    }
}

#[derive(Debug)]
struct Config {
    service_worker: ServiceWorker,
    labels: Labels,
    open_url: Option<String>,
    store: bool,
}

/// Loading state.
#[derive(Debug)]
enum Phase {
    Idle,
    Loading(Vec<oneshot::Sender<()>>),
    Ready,
}

/// Reloads of the schedule from `IndexedDB`, one at a time.
#[derive(Debug, Default)]
struct Reloads {
    running: bool,
    /// Another tab wrote while a reload ran: read again after it.
    again: bool,
    /// Lead once the next reload is applied (the lock or lease arrived).
    promote: bool,
}

/// A page notification and its handlers (dropping them detaches them).
struct PageNote {
    note: JsValue,
    _handlers: [Closure<dyn FnMut()>; 3],
}

impl PageNote {
    fn detach(&self) {
        for event in ["onclick", "onclose", "onerror"] {
            js::set(&self.note, event, &JsValue::NULL);
        }
    }

    /// Detaches it and drops its handlers on a later turn of the event
    /// loop: one of them may be running right now.
    fn retire(self) {
        self.detach();
        wasm_bindgen_futures::spawn_local(async move { drop(self) });
    }
}

/// A cached image URL: `data:` for the worker, Blob for the page.
type CacheKey = (ImageKey, bool);

/// JS objects of the host.
#[derive(Default)]
struct JsState {
    registration: Option<JsValue>,
    db: Option<JsValue>,
    page: HashMap<NotificationId, PageNote>,
    /// URL and users of each image.
    cache: HashMap<CacheKey, (String, u32)>,
    /// Image URLs of each shown notification and the cache entries they
    /// hold.
    urls: HashMap<NotificationId, (Urls, Vec<CacheKey>)>,
    listener: Option<Closure<dyn FnMut(JsValue)>>,
    /// The tabs' `BroadcastChannel` (schedule changes, the lease).
    channel: Option<JsValue>,
    /// `controllerchange` is heard (a worker may arrive later).
    watching_controller: bool,
}

impl JsState {
    fn acquire(
        &mut self,
        image: &crate::NotificationImage,
        blob: bool,
        held: &mut Vec<CacheKey>,
    ) -> Option<String> {
        match images::source(image)? {
            Source::Url(url) => Some(url.to_owned()),
            Source::Bytes(key, bytes) => {
                let key = (key, blob);
                if let Some((url, users)) = self.cache.get_mut(&key) {
                    *users += 1;
                    held.push(key);
                    return Some(url.clone());
                }
                let url = if blob {
                    blob_url(key.0.mime, bytes)?
                } else {
                    images::data_url(key.0.mime, bytes)
                };
                self.cache.insert(key, (url.clone(), 1));
                held.push(key);
                Some(url)
            }
        }
    }

    fn release(&mut self, keys: &[CacheKey]) {
        for key in keys {
            let Some((url, users)) = self.cache.get_mut(key) else {
                continue;
            };
            *users -= 1;
            if *users == 0 {
                if key.1
                    && let Some(url_api) = js::global_get("URL")
                {
                    let _revoked = js::call(&url_api, "revokeObjectURL", &[JsValue::from_str(url)]);
                }
                self.cache.remove(key);
            }
        }
    }

    /// Forgets the images of `id`.
    fn forget(&mut self, id: &NotificationId) {
        if let Some((_, keys)) = self.urls.remove(id) {
            self.release(&keys);
        }
    }
}

/// A Blob URL of `bytes`.
fn blob_url(mime: &str, bytes: &[u8]) -> Option<String> {
    let blob_ctor: js_sys::Function = js::global_get("Blob")?.dyn_into().ok()?;
    let options = js::object();
    js::set(&options, "type", &JsValue::from_str(mime));
    let parts = Array::of1(&Uint8Array::from(bytes));
    let blob = js_sys::Reflect::construct(&blob_ctor, &Array::of2(&parts, &options)).ok()?;
    js::call(&js::global_get("URL")?, "createObjectURL", &[blob])
        .ok()?
        .as_string()
}

/// A browser call, in order.
enum Op {
    Show {
        id: NotificationId,
        payload: Box<Payload>,
        /// Cache entries of the images it replaces (released once shown).
        old: Vec<CacheKey>,
        event: Option<NotificationEvent>,
        reply: Option<oneshot::Sender<Result<(), NotificationError>>>,
    },
    Close(Vec<NotificationId>),
}

pub(crate) struct Inner {
    me: Weak<Self>,
    state: Mutex<State>,
    browser: Mutex<Browser>,
    phase: Mutex<Phase>,
    runtime: OnceLock<Weak<Runtime>>,
    /// The `BroadcastChannel` lease, where Web Locks are missing.
    lease: Mutex<Option<Lease>>,
    reloads: Mutex<Reloads>,
    timer: Timeout,
    js: SendWrapper<RefCell<JsState>>,
    ops: flume::Sender<Op>,
    config: Config,
}

impl std::fmt::Debug for Inner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebNotifications")
            .field("config", &self.config)
            .field("browser", &*lock(&self.browser))
            .finish_non_exhaustive()
    }
}

/// Unix-epoch milliseconds from the browser's clock.
fn now() -> i64 {
    crate::builder::now_ms()
}

/// Whether this browser is known to refuse `new Notification()` (see
/// [`refuses_page_notifications`]).
fn page_refused_up_front() -> bool {
    let navigator = js::global_get("navigator").unwrap_or(JsValue::UNDEFINED);
    let hints = js::get(&navigator, "userAgentData");
    let hint_platform = hints
        .as_ref()
        .and_then(|h| js::get(h, "platform"))
        .and_then(|v| v.as_string());
    let user_agent = js::get(&navigator, "userAgent")
        .and_then(|v| v.as_string())
        .unwrap_or_default();
    refuses_page_notifications(&UserAgent {
        hint_mobile: hints
            .as_ref()
            .and_then(|h| js::get(h, "mobile"))
            .and_then(|v| v.as_bool()),
        hint_platform: hint_platform.as_deref(),
        text: &user_agent,
        touch: js::has(&js::global(), "ontouchstart"),
    })
}

/// What the browser offers, by feature detection.
fn detect(service_worker: bool) -> Browser {
    let Some(ctor) = js::global_get("Notification") else {
        return Browser::default();
    };
    let proto = js::get(&ctor, "prototype").unwrap_or(JsValue::UNDEFINED);
    let max_actions = if js::has(&proto, "actions") {
        js::get(&ctor, "maxActions")
            .and_then(|v| v.as_f64())
            .filter(|v| v.is_finite() && *v >= 0.0)
            .map_or(0, |v| {
                #[expect(
                    clippy::cast_possible_truncation,
                    clippy::cast_sign_loss,
                    reason = "checked finite and non-negative; clamped below"
                )]
                let n = v.min(f64::from(
                    u32::try_from(crate::validate::MAX_ACTIONS).unwrap_or(u32::MAX),
                )) as u32;
                n
            })
    } else {
        0
    };
    Browser {
        api: true,
        service_worker,
        page_refused: page_refused_up_front(),
        max_actions,
        image: js::has(&proto, "image"),
        require_interaction: js::has(&proto, "requireInteraction"),
        silent: js::has(&proto, "silent"),
        renotify: js::has(&proto, "renotify"),
        timestamp: js::has(&proto, "timestamp"),
    }
}

fn permission_state() -> PermissionState {
    let Some(ctor) = js::global_get("Notification") else {
        return PermissionState::Unavailable;
    };
    match js::get(&ctor, "permission")
        .and_then(|v| v.as_string())
        .as_deref()
    {
        Some("granted") => PermissionState::Granted,
        Some("denied") => PermissionState::Denied,
        _ => PermissionState::NotDetermined,
    }
}

fn platform(err: &JsValue) -> NotificationError {
    NotificationError::Platform(PlatformError::message(js::message(err)))
}

/// How this tab finds out whether it runs the schedule.
#[derive(Debug, PartialEq)]
enum Election {
    /// `navigator.locks`: an exclusive lock held for the tab's life.
    Locks(JsValue),
    /// No Web Locks: the `BroadcastChannel` lease.
    Lease,
    /// Neither (or no store): this tab schedules on its own.
    Alone,
}

/// `{istmo: MESSAGE, kind}` for the tabs' channel.
fn message(kind: &str) -> JsValue {
    let data = js::object();
    js::set(&data, "istmo", &JsValue::from_str(MESSAGE));
    js::set(&data, "kind", &JsValue::from_str(kind));
    data
}

fn post_message(channel: &JsValue, data: &JsValue) {
    if let Err(err) = js::call(channel, "postMessage", std::slice::from_ref(data)) {
        tracing::debug!(err = %js::message(&err), "notifications: tab message not sent");
    }
}

/// A lease message as sent on the channel.
fn lease_data(message: Message) -> JsValue {
    let (op, tab) = match message {
        Message::Beat(tab) => ("beat", tab),
        Message::Claim(tab) => ("claim", tab),
        Message::Resign(tab) => ("resign", tab),
    };
    let data = self::message("lease");
    js::set(&data, "op", &JsValue::from_str(op));
    js::set(&data, "tab", &JsValue::from(tab));
    data
}

/// The lease message in `data`, if it is one.
fn lease_message(data: &JsValue) -> Option<Message> {
    let tab = js::get(data, "tab")?.as_f64()?;
    if !(0.0..=f64::from(u32::MAX)).contains(&tab) || tab.fract() != 0.0 {
        return None;
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "checked integral and within u32"
    )]
    let tab = tab as u32;
    match js::get(data, "op")?.as_string()?.as_str() {
        "beat" => Some(Message::Beat(tab)),
        "claim" => Some(Message::Claim(tab)),
        "resign" => Some(Message::Resign(tab)),
        _ => None,
    }
}

/// `navigator.serviceWorker`.
fn container() -> Option<JsValue> {
    js::get(&js::global_get("navigator")?, "serviceWorker")
}

/// A promise resolving with `undefined` after `ms`.
pub(super) fn sleep(ms: i32) -> js_sys::Promise {
    js_sys::Promise::new(&mut |resolve, _reject| {
        let _timer = js::call(
            &js::global(),
            "setTimeout",
            &[resolve.into(), JsValue::from(ms)],
        );
    })
}

/// The JS options of `payload`; `persistent` adds the worker-only ones.
fn options_of(payload: &Payload, persistent: bool) -> JsValue {
    let o = js::object();
    let text = |key: &str, value: &Option<String>| {
        if let Some(value) = value {
            js::set(&o, key, &JsValue::from_str(value));
        }
    };
    text("body", &payload.body);
    text("icon", &payload.icon);
    js::set(&o, "tag", &JsValue::from_str(&payload.tag));
    if persistent {
        text("image", &payload.image);
        if !payload.actions.is_empty() {
            let actions: Array = payload
                .actions
                .iter()
                .map(|action| {
                    let a = js::object();
                    js::set(&a, "action", &JsValue::from_str(&action.key));
                    js::set(&a, "title", &JsValue::from_str(&action.title));
                    if let Some(icon) = &action.icon {
                        js::set(&a, "icon", &JsValue::from_str(icon));
                    }
                    a
                })
                .collect();
            js::set(&o, "actions", &actions);
        }
    }
    if payload.require_interaction {
        js::set(&o, "requireInteraction", &JsValue::TRUE);
    }
    if payload.silent {
        js::set(&o, "silent", &JsValue::TRUE);
    }
    if payload.renotify {
        js::set(&o, "renotify", &JsValue::TRUE);
    }
    if let Some(at) = payload.timestamp {
        #[expect(
            clippy::cast_precision_loss,
            reason = "epoch milliseconds fit f64 exactly"
        )]
        js::set(&o, "timestamp", &JsValue::from_f64(at as f64));
    }
    let data = js::object();
    js::set(&data, "istmo", &JsValue::from(DATA_VERSION));
    js::set(&data, "id", &JsValue::from_str(&payload.data.id));
    js::set(
        &data,
        "record",
        &Uint8Array::from(payload.data.record.as_slice()),
    );
    let background: Array = payload
        .data
        .background
        .iter()
        .map(|key| JsValue::from_str(key))
        .collect();
    js::set(&data, "background", &background);
    #[expect(
        clippy::cast_precision_loss,
        reason = "epoch milliseconds fit f64 exactly"
    )]
    js::set(
        &data,
        "delivered",
        &JsValue::from_f64(payload.data.delivered_ms as f64),
    );
    if let Some(open) = &payload.data.open {
        js::set(&data, "open", &JsValue::from_str(open));
    }
    js::set(&o, "data", &data);
    o
}

/// The plugin's `data` of a browser notification: id, record, delivered.
fn data_of(note: &JsValue) -> Option<(NotificationId, Option<Notification>, i64)> {
    let data = js::get(note, "data")?;
    if js::get(&data, "istmo").and_then(|v| v.as_f64()) != Some(f64::from(DATA_VERSION)) {
        return None;
    }
    let id = NotificationId::try_from(js::get(&data, "id")?.as_string()?.as_str()).ok()?;
    let record = js::get(&data, "record")
        .filter(JsValue::is_object)
        .and_then(|bytes| payload::read_record(&Uint8Array::new(&bytes).to_vec()));
    #[expect(
        clippy::cast_possible_truncation,
        reason = "the host wrote an integral epoch millisecond count"
    )]
    let delivered = js::get(&data, "delivered")
        .and_then(|v| v.as_f64())
        .map_or(0, |v| v as i64);
    Some((id, record, delivered))
}

impl Inner {
    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    fn me(&self) -> Arc<Self> {
        self.me.upgrade().expect("host alive while it runs")
    }

    fn runtime(&self) -> Option<Arc<Runtime>> {
        self.runtime.get().and_then(Weak::upgrade)
    }

    fn bind(&self, runtime: &Arc<Runtime>) {
        let _set = self.runtime.set(Arc::downgrade(runtime));
    }

    fn bind_ctx(&self, ctx: &HostCtx) {
        if self.runtime.get().is_none()
            && let Ok(runtime) = ctx.runtime()
        {
            self.bind(&runtime);
        }
    }

    /// Starts the load on first use; resolves once it finished.
    fn wait_ready(&self) -> impl Future<Output = ()> + Send + use<> {
        let rx = {
            let mut phase = lock(&self.phase);
            match &mut *phase {
                Phase::Ready => None,
                Phase::Loading(waiters) => {
                    let (tx, rx) = oneshot::channel();
                    waiters.push(tx);
                    Some(rx)
                }
                Phase::Idle => {
                    let (tx, rx) = oneshot::channel();
                    *phase = Phase::Loading(vec![tx]);
                    let me = self.me();
                    wasm_bindgen_futures::spawn_local(async move { me.load().await });
                    Some(rx)
                }
            }
        };
        async move {
            if let Some(rx) = rx {
                let _loaded = rx.await;
            }
        }
    }

    async fn ready(&self, ctx: &HostCtx) {
        self.bind_ctx(ctx);
        self.wait_ready().await;
    }

    fn set_browser(&self, browser: Browser) {
        *lock(&self.browser) = browser;
        self.state().set_limits(browser.limits(&self.config.labels));
    }

    fn browser(&self) -> Browser {
        *lock(&self.browser)
    }

    async fn load(self: Arc<Self>) {
        self.set_browser(detect(false));
        let registration = self.find_registration().await;
        if let Some(registration) = &registration {
            self.adopt_registration(registration.clone()).await;
        }
        let mut election = Election::Alone;
        if self.config.store
            && let Some(db) = idb::open().await
        {
            self.js.borrow_mut().db = Some(db);
            election = self.coordinate();
            if election != Election::Alone {
                // Another tab may run the schedule: follow until elected.
                self.state().set_leader(false);
            }
            self.reload().await;
            // Also when no worker was found yet: a tap may have been
            // queued while no page ran, or while this one loaded.
            self.drain().await;
        }
        let previous = std::mem::replace(&mut *lock(&self.phase), Phase::Ready);
        let waiters = match previous {
            Phase::Loading(waiters) => waiters,
            _ => Vec::new(),
        };
        for waiter in waiters {
            let _woken = waiter.send(());
        }
        self.persist();
        self.run_due();
        match election {
            Election::Locks(locks) => self.request_lock(&locks),
            Election::Lease => self.start_lease(),
            Election::Alone => {}
        }
    }

    /// The service worker registration to show with, per the config.
    async fn find_registration(&self) -> Option<JsValue> {
        let container = container()?;
        js::global_get("Notification")?;
        let existing = match js::call(&container, "getRegistration", &[]) {
            Ok(promise) => js::settle(promise).await.ok().filter(JsValue::is_object),
            Err(_) => None,
        };
        match &self.config.service_worker {
            ServiceWorker::None => return None,
            ServiceWorker::Existing => {
                if existing.is_none() {
                    tracing::info!(
                        "notifications: no service worker registered yet; page notifications until the app's worker takes control"
                    );
                    self.watch_controller(&container);
                    return None;
                }
            }
            ServiceWorker::Register(url) => {
                let foreign = existing.as_ref().and_then(|registration| {
                    let script = ["active", "waiting", "installing"]
                        .into_iter()
                        .find_map(|state| js::get(registration, state))
                        .and_then(|worker| js::get(&worker, "scriptURL"))
                        .and_then(|url| url.as_string())?;
                    let file = url.rsplit('/').next().unwrap_or(url);
                    (!script.ends_with(file)).then_some(script)
                });
                if let Some(script) = foreign {
                    tracing::info!(
                        %script,
                        "notifications: the page already has a service worker; using it (it must importScripts the plugin's)"
                    );
                } else {
                    // A missing script (404), a wrong MIME type or a scope
                    // the URL may not claim rejects here: fall back to page
                    // notifications at once instead of waiting for `ready`.
                    let registered =
                        match js::call(&container, "register", &[JsValue::from_str(url)]) {
                            Ok(promise) => js::settle(promise).await,
                            Err(err) => Err(err),
                        };
                    if let Err(err) = registered {
                        tracing::warn!(
                            err = %js::message(&err),
                            %url,
                            "notifications: service worker not registered; page notifications only (serve the plugin's script at this URL)"
                        );
                        return None;
                    }
                }
            }
        }
        let ready = js::get(&container, "ready")?;
        let race = js_sys::Promise::race(&Array::of2(&ready, &sleep(READY_WAIT_MS)));
        match js::settle(race.into()).await {
            Ok(registration) if registration.is_object() => Some(registration),
            Ok(_) => {
                tracing::warn!(
                    "notifications: service worker not active yet; page notifications until it is"
                );
                let me = self.me();
                wasm_bindgen_futures::spawn_local(async move {
                    if let Ok(registration) = js::settle(ready).await
                        && registration.is_object()
                    {
                        me.adopt_late(registration).await;
                    }
                });
                None
            }
            Err(err) => {
                tracing::warn!(err = %js::message(&err), "notifications: service worker failed");
                None
            }
        }
    }

    /// A worker that became active after the load: adopt it unless one
    /// already is.
    async fn adopt_late(&self, registration: JsValue) {
        if self.js.borrow().registration.is_some() {
            return;
        }
        self.adopt_registration(registration).await;
        self.drain().await;
    }

    /// `ServiceWorker::Existing` without a registration yet: look again
    /// whenever a worker takes control of the page.
    fn watch_controller(&self, container: &JsValue) {
        if std::mem::replace(&mut self.js.borrow_mut().watching_controller, true) {
            return;
        }
        let me = self.me.clone();
        let container_ = container.clone();
        let listener = Closure::<dyn FnMut()>::new(move || {
            let Some(inner) = me.upgrade() else {
                return;
            };
            let container = container_.clone();
            wasm_bindgen_futures::spawn_local(async move {
                let Ok(promise) = js::call(&container, "getRegistration", &[]) else {
                    return;
                };
                if let Ok(registration) = js::settle(promise).await
                    && registration.is_object()
                {
                    inner.adopt_late(registration).await;
                }
            });
        });
        let _added = js::call(
            container,
            "addEventListener",
            &[
                JsValue::from_str("controllerchange"),
                listener.into_js_value(),
            ],
        );
    }

    /// Starts using `registration`: notifications it shows that the
    /// table does not know are adopted, its messages are heard.
    async fn adopt_registration(&self, registration: JsValue) {
        self.set_browser(detect(true));
        self.js.borrow_mut().registration = Some(registration.clone());
        self.listen();
        if let Ok(listed) = self.listed(&registration).await {
            let mut state = self.state();
            for (_, record, delivered) in listed {
                if let Some(record) = record {
                    state.adopt(record, delivered);
                }
            }
        }
        self.rearm();
    }

    /// The plugin's notifications the worker shows.
    async fn listed(
        &self,
        registration: &JsValue,
    ) -> Result<Vec<(NotificationId, Option<Notification>, i64)>, JsValue> {
        let list = js::settle(js::call(registration, "getNotifications", &[])?).await?;
        Ok(Array::from(&list)
            .iter()
            .filter_map(|note| data_of(&note))
            .collect())
    }

    fn listen(&self) {
        let Some(container) = container() else {
            return;
        };
        if self.js.borrow().listener.is_some() {
            return;
        }
        let me = self.me.clone();
        let listener = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
            let Some(data) = js::get(&event, "data") else {
                return;
            };
            if js::get(&data, "istmo")
                .and_then(|v| v.as_string())
                .as_deref()
                != Some(MESSAGE)
            {
                return;
            }
            if let Some(inner) = me.upgrade() {
                wasm_bindgen_futures::spawn_local(async move { inner.drain().await });
            }
        });
        let _added = js::call(
            &container,
            "addEventListener",
            &[JsValue::from_str("message"), listener.as_ref().clone()],
        );
        // Messages wait in the client queue until it is started.
        let _started = js::call(&container, "startMessages", &[]);
        self.js.borrow_mut().listener = Some(listener);
    }

    /// Handles the clicks and closes the worker queued.
    async fn drain(&self) {
        let Some(db) = self.js.borrow().db.clone() else {
            return;
        };
        let entries = match idb::drain(&db).await {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(err = %js::message(&err), "notifications: response queue not read");
                return;
            }
        };
        for entry in entries {
            let text = |key| js::get(&entry, key).and_then(|v| v.as_string());
            let Some(id) = text("id").and_then(|id| NotificationId::try_from(id.as_str()).ok())
            else {
                continue;
            };
            let record = js::get(&entry, "record")
                .filter(JsValue::is_object)
                .and_then(|bytes| payload::read_record(&Uint8Array::new(&bytes).to_vec()));
            match text("kind").as_deref() {
                Some("click") => {
                    let action = text("action").filter(|a| !a.is_empty());
                    let launched = js::get(&entry, "launched").is_some_and(|v| v.is_truthy());
                    self.clicked(&id, action.as_deref(), launched, record);
                }
                Some("close") => self.closed(&id, record),
                _ => {}
            }
        }
    }

    /// A click on `id` (body when `key` is `None`).
    fn clicked(
        &self,
        id: &NotificationId,
        key: Option<&str>,
        launched: bool,
        record: Option<Notification>,
    ) {
        let outcome = self.state().activated(id, key, launched, record);
        self.js.borrow_mut().forget(id);
        match outcome {
            Some(Activated::Respond(response)) => {
                if let Some(runtime) = self.runtime() {
                    if let Err(err) = NotificationResponses::publish(&runtime, &response) {
                        tracing::warn!(%err, "notifications: response not published");
                    }
                } else {
                    tracing::warn!("notifications: response before the host was attached; dropped");
                }
            }
            Some(Activated::Snooze(notification, ms)) => {
                self.remove_ids(std::slice::from_ref(id));
                self.state().snooze(*notification, ms, now());
                self.persist();
                self.rearm();
            }
            None => tracing::debug!(id = %id, "notifications: click with an unknown action key"),
        }
    }

    fn closed(&self, id: &NotificationId, record: Option<Notification>) {
        self.state().dismissed(id, DismissReason::User, record);
        self.js.borrow_mut().forget(id);
        let note = self.js.borrow_mut().page.remove(id);
        if let Some(note) = note {
            note.retire();
        }
    }

    fn rearm(&self) {
        let deadline = self.state().next_deadline();
        self.timer.arm(deadline);
    }

    /// Writes the records changed since the last write to `IndexedDB`
    /// (one per entry, never the whole schedule) and tells the other
    /// tabs to reload.
    fn persist(&self) {
        if !matches!(*lock(&self.phase), Phase::Ready) {
            return;
        }
        let writes = self.state().take_writes();
        if writes.is_empty() {
            return;
        }
        let Some(db) = self.js.borrow().db.clone().filter(|_| self.config.store) else {
            // In memory only: nothing to write.
            return;
        };
        let writes: Vec<(NotificationId, Option<Vec<u8>>)> = writes
            .into_iter()
            .filter_map(|(id, record)| match record {
                Ok(record) => Some((id, record)),
                Err(err) => {
                    tracing::warn!(%err, id = %id, "notifications: scheduled entry not encoded");
                    None
                }
            })
            .collect();
        let channel = self.js.borrow().channel.clone();
        // Tasks start in spawn order and each creates its transaction
        // before its first await: writes land in order.
        wasm_bindgen_futures::spawn_local(async move {
            match idb::write_records(&db, &writes).await {
                Ok(()) => {
                    if let Some(channel) = channel {
                        post_message(&channel, &message("changed"));
                    }
                }
                Err(err) => {
                    tracing::warn!(err = %js::message(&err), "notifications: schedule not stored");
                }
            }
        });
    }

    /// Reads the schedule again (another tab wrote, or this one was
    /// elected). Reloads run one at a time; one asked for while another
    /// runs follows it.
    async fn reload(&self) {
        {
            let mut reloads = lock(&self.reloads);
            if reloads.running {
                reloads.again = true;
                return;
            }
            reloads.running = true;
        }
        loop {
            let db = self.js.borrow().db.clone();
            if let Some(db) = db {
                let mark = self.state().edit_mark();
                match idb::load_records(&db).await {
                    Ok(records) => {
                        let (entries, errors) = decode_records(records.iter().map(Vec::as_slice));
                        for err in errors {
                            tracing::warn!(%err, "notifications: stored entry unreadable; skipped");
                        }
                        self.state().replace_entries(mark, entries);
                    }
                    Err(err) => {
                        tracing::warn!(err = %js::message(&err), "notifications: stored schedule not read");
                    }
                }
            }
            let mut reloads = lock(&self.reloads);
            if std::mem::take(&mut reloads.again) {
                continue;
            }
            reloads.running = false;
            if std::mem::take(&mut reloads.promote) {
                self.state().set_leader(true);
                tracing::debug!("notifications: this tab runs the schedule");
            }
            break;
        }
        self.run_due();
        self.rearm();
    }

    /// This tab was elected: reload, then run the schedule (missed
    /// entries follow their policy here, in the leader only).
    async fn lead(&self) {
        lock(&self.reloads).promote = true;
        self.reload().await;
    }

    /// Sets up the tabs' channel; how this tab gets elected.
    fn coordinate(&self) -> Election {
        let channel = js::global_get("BroadcastChannel")
            .and_then(|ctor| ctor.dyn_into::<js_sys::Function>().ok())
            .and_then(|ctor| {
                js_sys::Reflect::construct(&ctor, &Array::of1(&JsValue::from_str(CHANNEL_NAME)))
                    .ok()
            });
        if let Some(channel) = &channel {
            let me = self.me.clone();
            let listener = Closure::<dyn FnMut(JsValue)>::new(move |event: JsValue| {
                if let Some(inner) = me.upgrade()
                    && let Some(data) = js::get(&event, "data")
                {
                    inner.heard(&data);
                }
            });
            let _added = js::call(
                channel,
                "addEventListener",
                &[JsValue::from_str("message"), listener.into_js_value()],
            );
            self.js.borrow_mut().channel = Some(channel.clone());
        }
        let locks = js::global_get("navigator")
            .and_then(|navigator| js::get(&navigator, "locks"))
            .filter(|locks| js::has(locks, "request"));
        match (locks, channel) {
            (Some(locks), _) => Election::Locks(locks),
            (None, Some(_)) => Election::Lease,
            (None, None) => {
                tracing::info!(
                    "notifications: no Web Locks or BroadcastChannel; each tab runs its own schedule"
                );
                Election::Alone
            }
        }
    }

    /// A message from another tab.
    fn heard(&self, data: &JsValue) {
        if js::get(data, "istmo")
            .and_then(|v| v.as_string())
            .as_deref()
            != Some(MESSAGE)
        {
            return;
        }
        match js::get(data, "kind").and_then(|v| v.as_string()).as_deref() {
            Some("changed") => {
                let me = self.me();
                wasm_bindgen_futures::spawn_local(async move { me.reload().await });
            }
            Some("lease") => {
                let Some(message) = lease_message(data) else {
                    return;
                };
                let step = lock(&self.lease)
                    .as_mut()
                    .and_then(|lease| lease.receive(message, now()));
                self.lease_steps(step.into_iter().collect());
            }
            _ => {}
        }
    }

    /// Asks for the scheduler lock; held for the page's life (released
    /// by the browser when the tab closes).
    fn request_lock(&self, locks: &JsValue) {
        let me = self.me.clone();
        let granted = Closure::<dyn FnMut(JsValue) -> JsValue>::new(move |_lock: JsValue| {
            if let Some(inner) = me.upgrade() {
                wasm_bindgen_futures::spawn_local(async move { inner.lead().await });
            }
            // Never settles: the lock stays held.
            js_sys::Promise::new(&mut |_resolve, _reject| {}).into()
        });
        let options = js::object();
        js::set(&options, "mode", &JsValue::from_str("exclusive"));
        let requested = js::call(
            locks,
            "request",
            &[
                JsValue::from_str(LOCK_NAME),
                options,
                granted.into_js_value(),
            ],
        );
        let me = self.me();
        wasm_bindgen_futures::spawn_local(async move {
            let settled = match requested {
                Ok(promise) => js::settle(promise).await.map(drop),
                Err(err) => Err(err),
            };
            if let Err(err) = settled {
                // E.g. a sandboxed or opaque origin.
                tracing::warn!(err = %js::message(&err), "notifications: Web Locks refused; electing over BroadcastChannel");
                if me.js.borrow().channel.is_some() {
                    me.start_lease();
                } else {
                    me.lead().await;
                }
            }
        });
    }

    /// Starts the `BroadcastChannel` lease ([`super::lease`]).
    fn start_lease(&self) {
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "Math.random() is in [0, 1): the product fits u32"
        )]
        let me_id = (js_sys::Math::random() * f64::from(u32::MAX)) as u32;
        *lock(&self.lease) = Some(Lease::new(me_id));
        let me = self.me.clone();
        let tick = Closure::<dyn FnMut()>::new(move || {
            if let Some(inner) = me.upgrade() {
                let steps = lock(&inner.lease)
                    .as_mut()
                    .map(|lease| lease.tick(now()))
                    .unwrap_or_default();
                inner.lease_steps(steps);
            }
        });
        #[expect(
            clippy::cast_precision_loss,
            reason = "a small constant number of milliseconds"
        )]
        let every = JsValue::from_f64((super::lease::CLAIM_MS / 2) as f64);
        let _interval = js::call(&js::global(), "setInterval", &[tick.into_js_value(), every]);
        let me = self.me.clone();
        let resign = Closure::<dyn FnMut()>::new(move || {
            if let Some(inner) = me.upgrade() {
                let message = lock(&inner.lease).as_mut().and_then(Lease::resign);
                if let Some(message) = message {
                    inner.state().set_leader(false);
                    inner.lease_steps(vec![Step::Send(message)]);
                }
            }
        });
        let _added = js::call(
            &js::global(),
            "addEventListener",
            &[JsValue::from_str("pagehide"), resign.into_js_value()],
        );
    }

    fn lease_steps(&self, steps: Vec<Step>) {
        for step in steps {
            match step {
                Step::Send(message) => {
                    let channel = self.js.borrow().channel.clone();
                    if let Some(channel) = channel {
                        post_message(&channel, &lease_data(message));
                    }
                }
                Step::Lead => {
                    let me = self.me();
                    wasm_bindgen_futures::spawn_local(async move { me.lead().await });
                }
                Step::Follow => {
                    tracing::debug!("notifications: another tab runs the schedule");
                    self.state().set_leader(false);
                    self.rearm();
                }
            }
        }
    }

    fn options(&self) -> Options {
        let browser = self.browser();
        Options {
            require_interaction: browser.require_interaction,
            silent: browser.silent,
            renotify: browser.renotify,
            timestamp: browser.timestamp,
            image: browser.image && browser.service_worker,
        }
    }

    /// Records `post` and queues it for the browser; the receiver gets
    /// the browser's answer.
    fn post(
        &self,
        post: Post,
        event: Option<NotificationEvent>,
    ) -> oneshot::Receiver<Result<(), NotificationError>> {
        let (reply, answer) = oneshot::channel();
        let now = now();
        let id = post.notification.id.clone();
        let delivered = self.state().delivered_ms(&id, post.mode, now);
        let blob = !self.browser().service_worker;
        let (urls, old) = {
            let mut js = self.js.borrow_mut();
            let previous = js.urls.get(&id).map(|(urls, _)| urls.clone());
            match previous {
                Some(urls) if !post.fresh_images => (urls, Vec::new()),
                _ => {
                    let mut held = Vec::new();
                    let wanted = images::wanted(&post.notification);
                    let icon = wanted
                        .icon
                        .and_then(|image| js.acquire(image, blob, &mut held));
                    let image = wanted
                        .image
                        .and_then(|image| js.acquire(image, blob, &mut held));
                    let actions = post
                        .notification
                        .actions
                        .iter()
                        .filter_map(|action| {
                            let url = js.acquire(action.icon.as_ref()?, blob, &mut held)?;
                            Some((action.id.value.clone(), url))
                        })
                        .collect();
                    let urls = Urls {
                        icon,
                        image,
                        actions,
                    };
                    let old = js
                        .urls
                        .insert(id.clone(), (urls.clone(), held))
                        .map(|(_, keys)| keys)
                        .unwrap_or_default();
                    (urls, old)
                }
            }
        };
        let payload = payload::build(
            &post,
            urls,
            self.options(),
            delivered,
            self.config.open_url.as_deref(),
        );
        self.state().posted(post, delivered, now);
        let op = Op::Show {
            id,
            payload: Box::new(payload),
            old,
            event,
            reply: Some(reply),
        };
        if self.ops.send(op).is_err() {
            tracing::warn!("notifications: browser queue closed");
        }
        answer
    }

    /// Posts and waits for the browser.
    async fn post_wait(
        &self,
        post: Post,
        event: Option<NotificationEvent>,
    ) -> Result<(), NotificationError> {
        let answer = self.post(post, event);
        self.rearm();
        answer.await.unwrap_or_else(|_| {
            Err(NotificationError::Platform(PlatformError::message(
                "host gone",
            )))
        })
    }

    async fn show_now(
        &self,
        notification: Notification,
        late: bool,
    ) -> Result<(), NotificationError> {
        let browser = self.browser();
        if !browser.can_show() {
            return Err(NotificationError::Unsupported(Feature::Show));
        }
        if permission_state() != PermissionState::Granted {
            return Err(NotificationError::PermissionDenied);
        }
        let post = self.state().prepare_show(notification, now())?;
        let id = post.notification.id.clone();
        let event = if late {
            NotificationEvent::DeliveredLate(id)
        } else {
            NotificationEvent::Delivered(id)
        };
        self.post_wait(post, Some(event)).await
    }

    async fn change(
        &self,
        id: &NotificationId,
        urgency: Urgency,
        touches_images: bool,
        change: impl FnOnce(&mut Notification) -> Result<(), NotificationError>,
    ) -> Result<(), NotificationError> {
        let post = self
            .state()
            .change(id, urgency, touches_images, change, now())?;
        if let Some(post) = post {
            self.post_wait(post, None).await
        } else {
            self.rearm();
            Ok(())
        }
    }

    /// Takes `ids` off the screen and out of the schedule.
    fn remove_ids(&self, ids: &[NotificationId]) {
        let shown = self.state().remove(ids);
        {
            let mut js = self.js.borrow_mut();
            for id in ids {
                js.forget(id);
            }
        }
        self.persist();
        if !shown.is_empty() && self.ops.send(Op::Close(shown)).is_err() {
            tracing::warn!("notifications: browser queue closed");
        }
        self.rearm();
    }

    /// Runs what is due: scheduled deliveries, queued updates, timeouts.
    /// The leader re-reads the device's zone first (it may change while
    /// the page is open; jiff caches the lookup).
    fn run_due(&self) {
        if !matches!(*lock(&self.phase), Phase::Ready) {
            return;
        }
        let now = now();
        let tick = {
            let mut state = self.state();
            if state.is_leader() {
                state.set_zone(TimeZone::system(), now);
            }
            state.tick(now)
        };
        self.persist();
        for due in tick.due {
            let me = self.me();
            wasm_bindgen_futures::spawn_local(async move {
                let id = due.notification.id.clone();
                if let Err(err) = me.show_now(due.notification, due.late).await {
                    me.state().failed(Some(&id), err.to_string());
                }
            });
        }
        for post in tick.queued {
            let _answer = self.post(post, None);
        }
        for id in tick.expired {
            self.remove_ids(std::slice::from_ref(&id));
            self.state()
                .emit(NotificationEvent::Dismissed(crate::Dismissal {
                    id,
                    reason: DismissReason::Timeout,
                }));
        }
        self.rearm();
    }

    /// One browser call.
    async fn run_op(&self, op: Op) {
        match op {
            Op::Show {
                id,
                payload,
                old,
                event,
                reply,
            } => {
                let result = self.show_in_browser(&id, &payload).await;
                {
                    let mut js = self.js.borrow_mut();
                    js.release(&old);
                    if result.is_err() {
                        js.forget(&id);
                    }
                }
                match &result {
                    Ok(()) => {
                        if let Some(event) = event {
                            self.state().emit(event);
                        }
                    }
                    Err(err) => {
                        let mut state = self.state();
                        state.remove(std::slice::from_ref(&id));
                        if reply.is_none() {
                            state.failed(Some(&id), err.to_string());
                        }
                    }
                }
                // Nobody waits any more: report the failure as an event.
                if let Some(reply) = reply
                    && let Err(unsent) = reply.send(result)
                    && let Err(err) = unsent.into_inner()
                {
                    self.state().failed(Some(&id), err.to_string());
                }
            }
            Op::Close(ids) => self.close_in_browser(&ids).await,
        }
    }

    async fn show_in_browser(
        &self,
        id: &NotificationId,
        payload: &Payload,
    ) -> Result<(), NotificationError> {
        let registration = self.js.borrow().registration.clone();
        if let Some(registration) = registration {
            let options = options_of(payload, true);
            let shown = js::call(
                &registration,
                "showNotification",
                &[JsValue::from_str(&payload.title), options],
            );
            return match shown {
                Ok(promise) => js::settle(promise).await.map(drop),
                Err(err) => Err(err),
            }
            .map_err(|err| {
                if js::is_type_error(&err) && permission_state() != PermissionState::Granted {
                    NotificationError::PermissionDenied
                } else {
                    platform(&err)
                }
            });
        }
        self.show_on_page(id, payload)
    }

    fn show_on_page(
        &self,
        id: &NotificationId,
        payload: &Payload,
    ) -> Result<(), NotificationError> {
        let ctor: js_sys::Function = js::global_get("Notification")
            .and_then(|ctor| ctor.dyn_into().ok())
            .ok_or(NotificationError::Unsupported(Feature::Show))?;
        let args = Array::of2(
            &JsValue::from_str(&payload.title),
            &options_of(payload, false),
        );
        let note = match js_sys::Reflect::construct(&ctor, &args) {
            Ok(note) => note,
            Err(err)
                if js::is_type_error(&err) && permission_state() == PermissionState::Granted =>
            {
                // Chrome on Android: page notifications are refused; only
                // a service worker can show.
                tracing::warn!(err = %js::message(&err), "notifications: page notifications refused; a service worker is needed here");
                let mut browser = self.browser();
                browser.page_refused = true;
                self.set_browser(browser);
                return Err(NotificationError::Unsupported(Feature::Show));
            }
            Err(err) if permission_state() != PermissionState::Granted => {
                tracing::debug!(err = %js::message(&err), "notifications: not permitted");
                return Err(NotificationError::PermissionDenied);
            }
            Err(err) => return Err(platform(&err)),
        };
        let handler = |kind: u8| {
            let me = self.me.clone();
            let id = id.clone();
            let note = note.clone();
            Closure::<dyn FnMut()>::new(move || {
                let Some(inner) = me.upgrade() else {
                    return;
                };
                match kind {
                    0 => {
                        let page = inner.js.borrow_mut().page.remove(&id);
                        if let Some(page) = page {
                            page.retire();
                        }
                        let _closed = js::call(&note, "close", &[]);
                        let _focused = js::call(&js::global(), "focus", &[]);
                        inner.clicked(&id, None, false, None);
                    }
                    1 => inner.closed(&id, None),
                    _ => inner.state().failed(
                        Some(&id),
                        "the browser could not show the notification".to_owned(),
                    ),
                }
            })
        };
        let handlers = [handler(0), handler(1), handler(2)];
        js::set(&note, "onclick", handlers[0].as_ref());
        js::set(&note, "onclose", handlers[1].as_ref());
        js::set(&note, "onerror", handlers[2].as_ref());
        let previous = self.js.borrow_mut().page.insert(
            id.clone(),
            PageNote {
                note,
                _handlers: handlers,
            },
        );
        if let Some(previous) = previous {
            // Replaced by tag; its close (if any) is not a dismissal.
            previous.retire();
        }
        Ok(())
    }

    async fn close_in_browser(&self, ids: &[NotificationId]) {
        let (registration, page) = {
            let mut js = self.js.borrow_mut();
            let page: Vec<PageNote> = ids.iter().filter_map(|id| js.page.remove(id)).collect();
            (js.registration.clone(), page)
        };
        for note in page {
            note.detach();
            let _closed = js::call(&note.note, "close", &[]);
        }
        let Some(registration) = registration else {
            return;
        };
        for id in ids {
            let filter = js::object();
            js::set(&filter, "tag", &JsValue::from_str(id.as_str()));
            let Ok(promise) = js::call(&registration, "getNotifications", &[filter]) else {
                continue;
            };
            let Ok(list) = js::settle(promise).await else {
                continue;
            };
            for note in Array::from(&list).iter() {
                if data_of(&note).is_some_and(|(of, ..)| of == *id) {
                    let _closed = js::call(&note, "close", &[]);
                }
            }
        }
    }

    async fn delivered_list(&self) -> Vec<DeliveredNotification> {
        let registration = self.js.borrow().registration.clone();
        let Some(registration) = registration else {
            return self.state().delivered();
        };
        match self.listed(&registration).await {
            Ok(listed) => {
                let mut state = self.state();
                let mut out = Vec::with_capacity(listed.len());
                for (id, record, delivered) in listed {
                    if let Some(record) = record {
                        out.push(super::state::delivered_of(&record, delivered));
                        state.adopt(record, delivered);
                    } else {
                        out.push(DeliveredNotification {
                            id,
                            title: None,
                            body: None,
                            group: None,
                            delivered_ms: Some(delivered),
                        });
                    }
                }
                out.sort_by(|a, b| (a.delivered_ms, &a.id).cmp(&(b.delivered_ms, &b.id)));
                out
            }
            Err(err) => {
                tracing::debug!(err = %js::message(&err), "notifications: getNotifications failed");
                self.state().delivered()
            }
        }
    }
}

/// Runs `f` on the event loop with the host and waits for it.
async fn on_loop<T, F, Fut>(inner: &Arc<Inner>, f: F) -> Option<T>
where
    T: Send + 'static,
    F: FnOnce(Arc<Inner>) -> Fut + Send + 'static,
    Fut: Future<Output = T> + 'static,
{
    let inner = Arc::clone(inner);
    js::local(move || f(inner)).await
}

impl Notifications for WebNotifications {
    async fn capabilities(&self, ctx: HostCtx) -> Capabilities {
        self.inner.ready(&ctx).await;
        self.inner.browser().capabilities()
    }

    async fn permission(&self, ctx: HostCtx) -> PermissionStatus {
        self.inner.bind_ctx(&ctx);
        PermissionStatus::of(permission_state())
    }

    /// `Notification.requestPermission()`. Firefox and Safari only ask
    /// during a user gesture: call it from a click handler (the hop
    /// through the runtime stays inside the gesture's activation
    /// window); elsewhere it answers the current state.
    async fn request_permission(
        &self,
        ctx: HostCtx,
        _request: PermissionRequest,
    ) -> Result<PermissionStatus, NotificationError> {
        self.inner.bind_ctx(&ctx);
        let asked = js::local(|| async {
            let ctor = js::global_get("Notification")?;
            // Old Safari only takes a callback (and returns `undefined`);
            // the others return a promise. Both settle this one.
            let asked = js_sys::Promise::new(&mut |resolve, reject| match js::call(
                &ctor,
                "requestPermission",
                &[resolve.clone().into()],
            ) {
                Ok(returned) if js::has(&returned, "then") => {
                    let _chained = js::call(&returned, "then", &[resolve.into(), reject.into()]);
                }
                Ok(_) => {}
                Err(err) => {
                    let _rejected = reject.call1(&JsValue::NULL, &err);
                }
            });
            js::settle(asked.into()).await.ok()?.as_string()
        })
        .await
        .flatten();
        if asked.is_none() && permission_state() == PermissionState::Unavailable {
            return Ok(PermissionStatus::of(PermissionState::Unavailable));
        }
        Ok(PermissionStatus::of(permission_state()))
    }

    async fn open_settings(
        &self,
        _ctx: HostCtx,
        _channel: Option<ChannelId>,
    ) -> Result<(), NotificationError> {
        Err(NotificationError::Unsupported(Feature::OpenSettings))
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
        self.inner.state().set_channels(channels);
        Ok(())
    }

    async fn delete_channel(&self, ctx: HostCtx, id: ChannelId) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        self.inner.state().delete_channel(&id);
        Ok(())
    }

    async fn show(
        &self,
        ctx: HostCtx,
        notification: Notification,
    ) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        on_loop(&self.inner, move |inner| async move {
            inner.show_now(notification, false).await
        })
        .await
        .unwrap_or(Err(NotificationError::Unsupported(Feature::Show)))
    }

    async fn schedule(
        &self,
        ctx: HostCtx,
        notification: Notification,
        trigger: Trigger,
    ) -> Result<ScheduledInfo, NotificationError> {
        self.inner.ready(&ctx).await;
        let info = self.inner.state().schedule(notification, trigger, now())?;
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
        self.inner.ready(&ctx).await;
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
        on_loop(&self.inner, move |inner| async move {
            inner
                .change(&id, urgency, touches_images, |n| {
                    n.apply(patch);
                    Ok(())
                })
                .await
        })
        .await
        .unwrap_or(Ok(()))
    }

    async fn set_progress(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        progress: Option<Progress>,
    ) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        on_loop(&self.inner, move |inner| async move {
            inner
                .change(&id, Urgency::Regular, false, |n| {
                    n.progress = progress;
                    Ok(())
                })
                .await
        })
        .await
        .unwrap_or(Ok(()))
    }

    async fn append_message(
        &self,
        ctx: HostCtx,
        id: NotificationId,
        message: ChatMessage,
    ) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        message.validate()?;
        let touches_images = message.image.is_some()
            || message
                .sender
                .as_ref()
                .is_some_and(|sender| sender.avatar.is_some());
        on_loop(&self.inner, move |inner| async move {
            inner
                .change(&id, Urgency::Alert, touches_images, |n| {
                    let Style::Messaging(conversation) = &mut n.style else {
                        return Err(InvalidField::new("style", Problem::NotAllowedHere).into());
                    };
                    conversation.messages.push(message);
                    Ok(())
                })
                .await
        })
        .await
        .unwrap_or(Ok(()))
    }

    async fn cancel(&self, ctx: HostCtx, id: NotificationId) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        self.inner.remove_ids(std::slice::from_ref(&id));
        Ok(())
    }

    async fn cancel_group(&self, ctx: HostCtx, group: GroupId) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        let ids = self.inner.state().group_ids(&group);
        self.inner.remove_ids(&ids);
        Ok(())
    }

    async fn cancel_all(&self, ctx: HostCtx) -> Result<(), NotificationError> {
        self.inner.ready(&ctx).await;
        let ids = self.inner.state().all_ids();
        self.inner.remove_ids(&ids);
        Ok(())
    }

    async fn pending(&self, ctx: HostCtx) -> Result<Vec<PendingNotification>, NotificationError> {
        self.inner.ready(&ctx).await;
        Ok(self.inner.state().pending())
    }

    async fn delivered(
        &self,
        ctx: HostCtx,
    ) -> Result<Vec<DeliveredNotification>, NotificationError> {
        self.inner.ready(&ctx).await;
        Ok(on_loop(
            &self.inner,
            |inner| async move { inner.delivered_list().await },
        )
        .await
        .unwrap_or_default())
    }

    async fn complete(&self, ctx: HostCtx, ack: AckToken) -> Result<(), NotificationError> {
        self.inner.bind_ctx(&ctx);
        if self.inner.state().complete(ack) {
            Ok(())
        } else {
            Err(NotificationError::NotFound(format!("ack {}", ack.value)))
        }
    }

    fn events(&self) -> flume::Receiver<NotificationEvent> {
        self.inner.state().subscribe()
    }
}
