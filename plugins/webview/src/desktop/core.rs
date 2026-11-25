//! Engine-independent part of the desktop hosts.
//!
//! A [`ViewCore`] owns everything a view needs that is not engine code:
//! the navigation rules, the event lanes, the pending decisions, scheme
//! requests and bridge replies, and the committed origin. Engines only
//! drive the native view on their UI thread and report what happens
//! through the `ViewCore` methods; answers flow back as closures the
//! engine runs on that thread.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use istmo_core::NativeHandleId;
use istmo_core::sync::lock;

use crate::host::{ControlLane, PendingTable, TelemetryLane, random_u64};
use crate::present::FrameSink;
use crate::rules::{CompiledRules, normalize_origin, origin_of};
use crate::{
    BridgeMessage, ClearDataRequest, ControlEvent, CookieFilter, DecisionKind, DecisionRequest,
    DecisionResponse, FindQuery, FindResult, HistoryNav, InterceptedNav, LoadRequest, NavErrorKind,
    NavFailure, NavInfo, ReplyResult, RuleAction, SchemeBody, SchemeRequestInfo,
    SchemeResponseHead, ScriptCall, SnapshotOptions, TelemetryEvent, TelemetryKind, ViewFrame,
    WebCookie, WebHistory, WebSnapshot, WebViewError, WebViewInitConfig, WebViewSpec,
};

/// Completion of an asynchronous engine call.
pub(crate) type Done<T> = Box<dyn FnOnce(Result<T, WebViewError>) + Send>;
/// Work for an engine's UI thread.
pub(crate) type UiTask = Box<dyn FnOnce() + Send>;
/// Work on one native view (`None` once closed).
pub(crate) type ViewTask = Box<dyn FnOnce(Option<&mut dyn UiView>) + Send>;
/// Settles a page's `istmo.invoke` promise on the UI thread.
pub(crate) type ReplyFn = Box<dyn FnOnce(ReplyResult) + Send>;

/// A desktop engine (`WebKitGTK`, `WKWebView`, `WebView2`, Qt `WebEngine`).
pub(crate) trait Engine: Send + Sync + 'static {
    /// Backend name and version for [`crate::WebViewCapabilities`].
    fn describe(&self) -> (String, String);
    /// Flip the capability flags this engine supports.
    fn capabilities(&self, caps: &mut crate::WebViewCapabilities);
    /// Run `task` on the engine's UI thread.
    fn post(&self, task: UiTask);
    /// Create the native view for `core` (on the UI thread); `done` when
    /// it exists.
    fn create(&self, core: Arc<ViewCore>, done: Done<()>);
    /// Run `f` with the native view `id` on the UI thread (`None` once
    /// closed).
    fn with_view(&self, id: NativeHandleId, f: ViewTask);
    /// In-process frame sink for view `id`.
    fn frame_sink(&self, id: NativeHandleId) -> Arc<dyn FrameSink>;
    /// Run `task` on the UI thread after `ms` milliseconds. The default
    /// queues it on the shared timer thread; engines with a native timer
    /// use it instead.
    fn post_after(self: Arc<Self>, ms: u32, task: UiTask) {
        super::timer::after(
            Duration::from_millis(ms.into()),
            Box::new(move || self.post(task)),
        );
    }
}

/// One native view, used on the UI thread only.
pub(crate) trait UiView {
    fn load(&mut self, request: &LoadRequest);
    fn load_html(&mut self, html: &str, base_url: Option<&str>);
    fn navigate(&mut self, nav: HistoryNav);
    fn history(&mut self) -> WebHistory;
    fn focus(&mut self);
    fn eval(&mut self, call: &ScriptCall, done: Done<String>);
    /// Deliver a bridge message to the main document.
    fn deliver(&mut self, channel: &str, data: &[u8]);
    fn cookies(&mut self, filter: &CookieFilter, done: Done<Vec<WebCookie>>);
    fn set_cookie(&mut self, cookie: &WebCookie, done: Done<()>);
    fn clear_data(&mut self, request: &ClearDataRequest, done: Done<()>);
    fn set_zoom(&mut self, factor: f32);
    fn find(&mut self, query: &FindQuery, done: Done<FindResult>);
    fn snapshot(&mut self, options: SnapshotOptions, done: Done<WebSnapshot>);
    fn print(&mut self) -> Result<(), WebViewError>;
    fn set_inspector(&mut self, open: bool) -> Result<(), WebViewError>;
    /// Tear down; the view is gone afterwards.
    fn close(&mut self);
}

/// Answers a dynamic scheme request; called on the UI thread.
pub(crate) trait SchemeResponder: Send {
    fn head(&mut self, head: &SchemeResponseHead);
    fn body(&mut self, body: SchemeBody);
}

/// A dynamic scheme request in flight. Calls are posted to the UI thread
/// in order, so chunks reach the responder in the order the app sent them.
pub(crate) struct SchemeSlot {
    responder: Mutex<Box<dyn SchemeResponder>>,
    answered: AtomicBool,
}

/// Resolves a decision on the UI thread.
pub(crate) type Resolver = Box<dyn FnOnce(DecisionResponse) + Send>;

/// Creates host-side state for views a page opens (`window.open`).
pub(crate) trait PopupMaker: Send + Sync {
    fn make(&self, spec: WebViewSpec) -> Option<(Arc<ViewCore>, crate::WebViewInstance)>;
}

/// Latest-value frame slot, read by the engine once per tick.
#[derive(Debug, Default)]
pub(crate) struct FrameSlot {
    frame: Mutex<Option<ViewFrame>>,
}

impl FrameSlot {
    pub(crate) fn store(&self, frame: ViewFrame) -> bool {
        lock(&self.frame).replace(frame) != Some(frame)
    }

    pub(crate) fn take(&self) -> Option<ViewFrame> {
        lock(&self.frame).take()
    }
}

/// Everything about a view that is not engine code.
pub(crate) struct ViewCore {
    pub(crate) id: NativeHandleId,
    pub(crate) spec: WebViewSpec,
    pub(crate) config: Arc<WebViewInitConfig>,
    pub(crate) control: ControlLane,
    pub(crate) telemetry: TelemetryLane,
    rules: CompiledRules,
    bridge_origins: Vec<String>,
    engine: Weak<dyn Engine>,
    asks: PendingTable<Resolver>,
    schemes: Mutex<HashMap<u64, Arc<SchemeSlot>>>,
    replies: PendingTable<(u64, ReplyFn)>,
    navigation: AtomicU64,
    document: AtomicU64,
    committed: Mutex<Option<String>>,
    last_progress: AtomicU64,
    popups: std::sync::OnceLock<Arc<dyn PopupMaker>>,
}

impl std::fmt::Debug for ViewCore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewCore")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl ViewCore {
    pub(crate) fn new(
        id: NativeHandleId,
        spec: WebViewSpec,
        config: Arc<WebViewInitConfig>,
        engine: Weak<dyn Engine>,
    ) -> Result<Self, WebViewError> {
        let rules = CompiledRules::new(&spec.rules)?;
        let bridge_origins = spec
            .bridge
            .as_ref()
            .map(|b| {
                b.allowed_origins
                    .iter()
                    .map(|o| normalize_origin(o))
                    .collect()
            })
            .transpose()?
            .unwrap_or_default();
        for script in &spec.user_scripts {
            for origin in &script.allowed_origins {
                normalize_origin(origin)?;
            }
        }
        Ok(Self {
            id,
            spec,
            config,
            control: ControlLane::default(),
            telemetry: TelemetryLane::default(),
            rules,
            bridge_origins,
            engine,
            asks: PendingTable::default(),
            schemes: Mutex::default(),
            replies: PendingTable::default(),
            navigation: AtomicU64::new(0),
            document: AtomicU64::new(0),
            committed: Mutex::new(None),
            last_progress: AtomicU64::new(u64::MAX),
            popups: std::sync::OnceLock::new(),
        })
    }

    fn post(&self, task: UiTask) {
        if let Some(engine) = self.engine.upgrade() {
            engine.post(task);
        }
    }

    fn post_after(&self, ms: u32, task: UiTask) {
        if let Some(engine) = self.engine.upgrade() {
            engine.post_after(ms, task);
        }
    }

    fn next_navigation(&self) -> u64 {
        self.navigation.fetch_add(1, Ordering::Relaxed) + 1
    }

    fn current_navigation(&self) -> u64 {
        self.navigation.load(Ordering::Relaxed)
    }

    fn info(&self, url: &str, gesture: bool) -> NavInfo {
        NavInfo {
            navigation_id: self.current_navigation(),
            url: url.to_owned(),
            is_redirect: false,
            user_gesture: gesture,
        }
    }

    /// Origin of the committed main document.
    pub(crate) fn committed_origin(&self) -> Option<String> {
        lock(&self.committed).clone()
    }

    pub(crate) fn document_id(&self) -> u64 {
        self.document.load(Ordering::Relaxed)
    }

    /// Whether `origin` may use the bridge.
    pub(crate) fn bridge_allows(&self, origin: &str, is_main_frame: bool) -> bool {
        let Some(bridge) = &self.spec.bridge else {
            return false;
        };
        (is_main_frame || bridge.allow_subframes) && self.bridge_origins.iter().any(|o| o == origin)
    }

    /// Normalised bridge origins, for engine-side injection filters.
    pub(crate) fn bridge_origins(&self) -> &[String] {
        &self.bridge_origins
    }

    // ---------------------------------------------------------------
    // Engine → core
    // ---------------------------------------------------------------

    /// A top-level navigation wants to start. `resolve(true)` lets it
    /// proceed; it may be called later (deferred `Ask`) from the UI
    /// thread. Engines that cannot defer pass `can_defer = false`: an
    /// `Ask` then cancels and re-issues the load once allowed.
    pub(crate) fn navigation(
        self: &Arc<Self>,
        url: &str,
        gesture: bool,
        resolve: Box<dyn FnOnce(bool) + Send>,
    ) {
        let navigation_id = self.next_navigation();
        let verdict = self
            .rules
            .evaluate(url, self.spec.settings.file_access)
            .action;
        let info = NavInfo {
            navigation_id,
            url: url.to_owned(),
            is_redirect: false,
            user_gesture: gesture,
        };
        match verdict {
            RuleAction::Allow => resolve(true),
            RuleAction::Deny => {
                resolve(false);
                self.failed(url, NavErrorKind::Cancelled, "blocked by navigation rules");
            }
            RuleAction::Intercept => {
                resolve(false);
                self.control
                    .publish(ControlEvent::Intercepted(InterceptedNav {
                        navigation_id,
                        url: url.to_owned(),
                    }));
            }
            RuleAction::External => {
                resolve(false);
                open_externally(url);
            }
            RuleAction::Ask => {
                let timeout = self.spec.timeouts.navigation_ms;
                self.ask(
                    DecisionKind::Navigation(info),
                    Box::new(move |response| resolve(response == DecisionResponse::Allow)),
                    Some(timeout),
                );
            }
        }
    }

    /// Rules for an app-issued load: `true` when the engine should load.
    /// `Ask` loads go through [`Self::navigation`] when the engine sees
    /// them, so they are let through here.
    pub(crate) fn admit_load(&self, url: &str) -> bool {
        match self
            .rules
            .evaluate(url, self.spec.settings.file_access)
            .action
        {
            RuleAction::Allow | RuleAction::Ask => true,
            RuleAction::Deny => {
                self.next_navigation();
                self.failed(url, NavErrorKind::Cancelled, "blocked by navigation rules");
                false
            }
            RuleAction::Intercept => {
                let navigation_id = self.next_navigation();
                self.control
                    .publish(ControlEvent::Intercepted(InterceptedNav {
                        navigation_id,
                        url: url.to_owned(),
                    }));
                false
            }
            RuleAction::External => {
                open_externally(url);
                false
            }
        }
    }

    pub(crate) fn set_popup_maker(&self, maker: Arc<dyn PopupMaker>) {
        drop(self.popups.set(maker));
    }

    /// Host state for a view this page opens, with the same config.
    pub(crate) fn new_popup(
        &self,
        spec: WebViewSpec,
    ) -> Option<(Arc<Self>, crate::WebViewInstance)> {
        self.popups.get()?.make(spec)
    }

    /// Whether a page-opened window to `url` is allowed by the rules
    /// alone (engines that need the answer synchronously).
    pub(crate) fn allows_popup(&self, url: &str) -> bool {
        self.rules
            .evaluate(url, self.spec.settings.file_access)
            .action
            == RuleAction::Allow
    }

    pub(crate) fn started(&self, url: &str) {
        self.control
            .publish(ControlEvent::NavigationStarted(self.info(url, false)));
        self.progress(0.0);
    }

    /// The main document changed: bump the document id, record the
    /// origin, drop replies owed to the old document.
    pub(crate) fn committed(&self, url: &str) {
        self.document.fetch_add(1, Ordering::Relaxed);
        let origin = url::Url::parse(url).ok().as_ref().and_then(origin_of);
        *lock(&self.committed) = origin;
        for (_, callback) in self.replies.drain() {
            self.post(Box::new(move || {
                callback(ReplyResult::Err("document replaced".into()));
            }));
        }
        self.control
            .publish(ControlEvent::NavigationCommitted(self.info(url, false)));
        if self.telemetry.wants(TelemetryKind::Url) {
            self.telemetry.publish(&TelemetryEvent::Url(url.to_owned()));
        }
    }

    pub(crate) fn finished(&self, url: &str, can_go_back: bool, can_go_forward: bool) {
        self.control
            .publish(ControlEvent::NavigationFinished(self.info(url, false)));
        self.progress(1.0);
        if self.telemetry.wants(TelemetryKind::History) {
            self.telemetry
                .publish(&TelemetryEvent::History(crate::HistoryState {
                    can_go_back,
                    can_go_forward,
                }));
        }
    }

    pub(crate) fn failed(&self, url: &str, kind: NavErrorKind, description: &str) {
        self.control
            .publish(ControlEvent::NavigationFailed(NavFailure {
                navigation_id: self.current_navigation(),
                url: url.to_owned(),
                kind,
                description: description.to_owned(),
            }));
    }

    /// Coalesced: one event per percent step at most.
    pub(crate) fn progress(&self, value: f64) {
        if !self.telemetry.wants(TelemetryKind::Progress) {
            return;
        }
        #[expect(
            clippy::cast_possible_truncation,
            clippy::cast_sign_loss,
            reason = "clamped to 0..=100"
        )]
        let percent = (value.clamp(0.0, 1.0) * 100.0) as u64;
        if self.last_progress.swap(percent, Ordering::Relaxed) != percent {
            #[expect(clippy::cast_possible_truncation, reason = "progress fraction")]
            self.telemetry
                .publish(&TelemetryEvent::Progress(value.clamp(0.0, 1.0) as f32));
        }
    }

    pub(crate) fn title(&self, title: &str) {
        if self.telemetry.wants(TelemetryKind::Title) {
            self.telemetry
                .publish(&TelemetryEvent::Title(title.to_owned()));
        }
    }

    pub(crate) fn emit(&self, event: ControlEvent) {
        self.control.publish(event);
    }

    /// Ask the app; `resolve` runs on the UI thread with the answer, or
    /// with `Deny` after `timeout_ms`.
    pub(crate) fn ask(
        self: &Arc<Self>,
        kind: DecisionKind,
        resolve: Resolver,
        timeout_ms: Option<u32>,
    ) {
        let request_id = self.asks.insert(resolve);
        self.control
            .publish(ControlEvent::DecisionRequested(DecisionRequest {
                request_id,
                kind,
            }));
        if let Some(ms) = timeout_ms.filter(|ms| *ms > 0) {
            let core = Arc::downgrade(self);
            self.post_after(
                ms,
                Box::new(move || {
                    if let Some(core) = core.upgrade()
                        && let Some(resolve) = core.asks.take(request_id)
                    {
                        resolve(DecisionResponse::Deny);
                    }
                }),
            );
        }
    }

    /// A bridge message from the page. `reply` settles an `invoke`.
    pub(crate) fn bridge_message(
        &self,
        channel: String,
        origin: String,
        is_main_frame: bool,
        document_id: u64,
        data: Vec<u8>,
        reply: Option<ReplyFn>,
    ) {
        let channel_ok = self
            .spec
            .bridge
            .as_ref()
            .is_some_and(|b| b.channels.contains(&channel));
        if !channel_ok
            || !self.bridge_allows(&origin, is_main_frame)
            || document_id != self.document_id()
        {
            if let Some(reply) = reply {
                reply(ReplyResult::Err("not allowed".into()));
            }
            return;
        }
        let reply_id = reply.map(|reply| self.replies.insert((document_id, reply)));
        self.control.publish(ControlEvent::Message(BridgeMessage {
            channel,
            origin,
            is_main_frame,
            document_id,
            reply_id,
            data,
        }));
    }

    /// A dynamic scheme request; `responder` answers it on the UI thread.
    /// Without a head within the timeout it gets `504`.
    pub(crate) fn scheme_request(
        self: &Arc<Self>,
        mut info: SchemeRequestInfo,
        responder: Box<dyn SchemeResponder>,
    ) {
        let slot = Arc::new(SchemeSlot {
            responder: Mutex::new(responder),
            answered: AtomicBool::new(false),
        });
        let request_id = {
            let mut schemes = lock(&self.schemes);
            let mut id = random_u64();
            while id == 0 || schemes.contains_key(&id) {
                id = random_u64();
            }
            schemes.insert(id, Arc::clone(&slot));
            id
        };
        info.request_id = request_id;
        info.initiator_origin = self.committed_origin();
        self.control.publish(ControlEvent::SchemeRequest(info));
        let core = Arc::downgrade(self);
        let ms = self.spec.timeouts.scheme_head_ms.max(1);
        self.post_after(
            ms,
            Box::new(move || {
                let Some(core) = core.upgrade() else { return };
                if slot.answered.swap(true, Ordering::AcqRel) {
                    return;
                }
                lock(&core.schemes).remove(&request_id);
                let mut responder = lock(&slot.responder);
                responder.head(&SchemeResponseHead {
                    request_id,
                    status: 504,
                    mime_type: "text/plain".into(),
                    headers: Vec::new(),
                    content_length: Some(0),
                });
                responder.body(SchemeBody::End);
            }),
        );
    }

    // ---------------------------------------------------------------
    // App → core (host methods)
    // ---------------------------------------------------------------

    pub(crate) fn decide(
        &self,
        request_id: u64,
        response: DecisionResponse,
    ) -> Result<(), WebViewError> {
        let resolve = self
            .asks
            .take(request_id)
            .ok_or(WebViewError::UnknownRequest)?;
        self.post(Box::new(move || resolve(response)));
        Ok(())
    }

    pub(crate) fn reply(&self, reply_id: u64, result: ReplyResult) -> Result<(), WebViewError> {
        let (document, callback) = self
            .replies
            .take(reply_id)
            .ok_or(WebViewError::UnknownRequest)?;
        if document != self.document_id() {
            return Err(WebViewError::UnknownRequest);
        }
        self.post(Box::new(move || callback(result)));
        Ok(())
    }

    fn scheme_slot(&self, request_id: u64, remove: bool) -> Result<Arc<SchemeSlot>, WebViewError> {
        let mut schemes = lock(&self.schemes);
        let slot = if remove {
            schemes.remove(&request_id)
        } else {
            schemes.get(&request_id).cloned()
        };
        slot.ok_or(WebViewError::UnknownRequest)
    }

    pub(crate) fn scheme_head(&self, head: SchemeResponseHead) -> Result<(), WebViewError> {
        let slot = self.scheme_slot(head.request_id, false)?;
        if slot.answered.swap(true, Ordering::AcqRel) {
            return Err(WebViewError::UnknownRequest);
        }
        self.post(Box::new(move || lock(&slot.responder).head(&head)));
        Ok(())
    }

    pub(crate) fn scheme_chunk(
        &self,
        request_id: u64,
        body: SchemeBody,
    ) -> Result<(), WebViewError> {
        let last = !matches!(body, SchemeBody::Data(_));
        let slot = self.scheme_slot(request_id, last)?;
        if !slot.answered.load(Ordering::Acquire) {
            if matches!(body, SchemeBody::Abort(_)) {
                slot.answered.store(true, Ordering::Release);
            } else {
                return Err(WebViewError::InvalidArgument("scheme_head first".into()));
            }
        }
        self.post(Box::new(move || lock(&slot.responder).body(body)));
        Ok(())
    }

    /// Tear down every pending request (close, crash).
    pub(crate) fn fail_pending(&self) {
        for resolve in self.asks.drain() {
            self.post(Box::new(move || resolve(DecisionResponse::Deny)));
        }
        let slots: Vec<_> = lock(&self.schemes).drain().map(|(_, slot)| slot).collect();
        for slot in slots {
            self.post(Box::new(move || {
                lock(&slot.responder).body(SchemeBody::Abort("view closed".into()));
            }));
        }
        for (_, callback) in self.replies.drain() {
            self.post(Box::new(move || {
                callback(ReplyResult::Err("view closed".into()));
            }));
        }
    }

    pub(crate) fn close(&self) {
        self.fail_pending();
        self.control.close();
        self.telemetry.close();
    }
}

/// Open `url` in the system's handler for it.
fn open_externally(url: &str) {
    #[cfg(all(target_arch = "wasm32", feature = "web"))]
    {
        super::web::open_external(url);
    }
    #[cfg(not(all(target_arch = "wasm32", feature = "web")))]
    open_with_system(url);
}

/// Hand `url` to the desktop's URL handler.
#[cfg(not(all(target_arch = "wasm32", feature = "web")))]
fn open_with_system(url: &str) {
    #[cfg(target_os = "macos")]
    let program = "open";
    #[cfg(target_os = "windows")]
    let program = "explorer";
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let program = "xdg-open";
    // One argv entry, no shell: nothing in the URL is interpreted.
    if let Err(err) = std::process::Command::new(program)
        .arg(url)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        tracing::warn!(%err, "could not open a link externally");
    }
}
