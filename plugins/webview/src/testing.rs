//! [`ScriptedWebView`]: an engine-less host for testing app flows (a
//! sign-in that intercepts its redirect, a bridge handler, a decision
//! prompt) in plain `cargo test`.
//!
//! The host applies the real navigation rules and token checks; page
//! behaviour is whatever the test scripts. Every call is recorded.
//!
//! ```ignore
//! let script = WebScript::default();
//! runtime.register_host(WebViewHost::new(ScriptedWebViewFactory::new(&runtime, script.clone())));
//! // drive the app, then:
//! script.emit(view_id, ControlEvent::Message(..));
//! assert!(script.calls().contains(&RecordedCall::Load("https://…".into())));
//! ```

use std::sync::{Arc, Mutex, Weak};

use istmo_core::sync::lock;
use istmo_core::{NativeHandleId, Runtime};

use crate::host::{ControlLane, PendingTable, TelemetryLane, ViewTable};
use crate::present::{FrameSink, register_frame_sink, unregister_frame_sink};
use crate::rules::CompiledRules;
use crate::{
    BridgeReply, ClearDataRequest, ControlEvent, CookieFilter, Decision, DecisionKind,
    DecisionRequest, DecisionResponse, DownloadOp, FindQuery, FindResult, HistoryEntry, HistoryNav,
    InterceptedNav, LoadRequest, NavErrorKind, NavFailure, NavInfo, OutgoingMessage, RuleAction,
    SchemeChunk, SchemeResponseHead, ScriptCall, SnapshotOptions, TelemetryEvent, TelemetryKind,
    ViewFrame, ViewRef, WebCookie, WebHistory, WebSnapshot, WebView, WebViewCapabilities,
    WebViewError, WebViewFactory, WebViewInitConfig, WebViewInstance, WebViewSpec,
};

/// A call the scripted host received.
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    missing_docs,
    reason = "each variant records the trait method of the same name"
)]
pub enum RecordedCall {
    Open(WebViewSpec),
    Close(NativeHandleId),
    Load(String),
    LoadHtml(String),
    History(HistoryNav),
    Eval(ScriptCall),
    PostMessage(OutgoingMessage),
    Reply(BridgeReply),
    Decide(Decision),
    SchemeHead(SchemeResponseHead),
    SchemeChunk(SchemeChunk),
    Frame(NativeHandleId, ViewFrame),
    SetCookie(WebCookie),
    ClearData(ClearDataRequest),
    Released(NativeHandleId),
}

/// Test-side control of every [`ScriptedWebView`] built from the same
/// factory. Cheap to clone.
#[derive(Clone, Default)]
pub struct WebScript {
    state: Arc<ScriptState>,
}

impl std::fmt::Debug for WebScript {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebScript").finish_non_exhaustive()
    }
}

type EvalHandler = Box<dyn Fn(&ScriptCall) -> Result<String, WebViewError> + Send + Sync>;

#[derive(Default)]
struct ScriptState {
    views: ViewTable<ScriptedView>,
    calls: Mutex<Vec<RecordedCall>>,
    cookies: Mutex<Vec<WebCookie>>,
    eval: Mutex<Option<EvalHandler>>,
}

impl WebScript {
    /// Every call so far, in order.
    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        lock(&self.state.calls).clone()
    }

    /// Push a control event to a view, as its page would.
    ///
    /// # Errors
    /// [`WebViewError::ViewClosed`] when no such view is open.
    pub fn emit(&self, view: NativeHandleId, event: ControlEvent) -> Result<(), WebViewError> {
        let view = self.view(view)?;
        view.control.publish(event);
        Ok(())
    }

    /// Push a telemetry event to a view.
    ///
    /// # Errors
    /// [`WebViewError::ViewClosed`] when no such view is open.
    pub fn emit_telemetry(
        &self,
        view: NativeHandleId,
        event: &TelemetryEvent,
    ) -> Result<(), WebViewError> {
        self.view(view)?.telemetry.publish(event);
        Ok(())
    }

    /// Answer `eval` calls with `handler` (default: `"null"`).
    pub fn on_eval(
        &self,
        handler: impl Fn(&ScriptCall) -> Result<String, WebViewError> + Send + Sync + 'static,
    ) {
        *lock(&self.state.eval) = Some(Box::new(handler));
    }

    /// Ids of the open views.
    #[must_use]
    pub fn open_views(&self) -> Vec<NativeHandleId> {
        self.state.views.ids()
    }

    fn view(&self, id: NativeHandleId) -> Result<Arc<ScriptedView>, WebViewError> {
        self.state.views.by_id(id).ok_or(WebViewError::ViewClosed)
    }

    fn record(&self, call: RecordedCall) {
        lock(&self.state.calls).push(call);
    }
}

/// One scripted view.
struct ScriptedView {
    control: ControlLane,
    telemetry: TelemetryLane,
    rules: CompiledRules,
    file_access: bool,
    history: Mutex<WebHistory>,
    /// Navigations waiting for an `Ask` decision.
    asks: PendingTable<String>,
    next_navigation: std::sync::atomic::AtomicU64,
}

impl ScriptedView {
    fn navigation_id(&self) -> u64 {
        self.next_navigation
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    }

    /// Run a navigation to `url` through the rules.
    fn navigate(&self, url: &str) {
        let navigation_id = self.navigation_id();
        let info = NavInfo {
            navigation_id,
            url: url.to_owned(),
            is_redirect: false,
            user_gesture: false,
        };
        match self.rules.evaluate(url, self.file_access).action {
            RuleAction::Allow => self.commit(info),
            RuleAction::Intercept => {
                self.control
                    .publish(ControlEvent::Intercepted(InterceptedNav {
                        navigation_id,
                        url: url.to_owned(),
                    }));
            }
            RuleAction::Ask => {
                let request_id = self.asks.insert(url.to_owned());
                self.control
                    .publish(ControlEvent::DecisionRequested(DecisionRequest {
                        request_id,
                        kind: DecisionKind::Navigation(info),
                    }));
            }
            RuleAction::Deny | RuleAction::External => {
                self.control
                    .publish(ControlEvent::NavigationFailed(NavFailure {
                        navigation_id,
                        url: url.to_owned(),
                        kind: NavErrorKind::Cancelled,
                        description: "blocked by navigation rules".into(),
                    }));
            }
        }
    }

    fn commit(&self, info: NavInfo) {
        self.control
            .publish(ControlEvent::NavigationStarted(info.clone()));
        {
            let mut history = lock(&self.history);
            let keep = history.entries.len().min(history.current as usize + 1);
            history.entries.truncate(keep);
            history.entries.push(HistoryEntry {
                url: info.url.clone(),
                title: String::new(),
            });
            history.current = u32::try_from(history.entries.len() - 1).unwrap_or(u32::MAX);
        }
        self.control
            .publish(ControlEvent::NavigationCommitted(info.clone()));
        if self.telemetry.wants(TelemetryKind::Url) {
            self.telemetry
                .publish(&TelemetryEvent::Url(info.url.clone()));
        }
        self.control.publish(ControlEvent::NavigationFinished(info));
    }
}

/// Builds [`ScriptedWebView`]s sharing one [`WebScript`].
#[derive(Debug)]
pub struct ScriptedWebViewFactory {
    script: WebScript,
    runtime: Weak<Runtime>,
}

impl ScriptedWebViewFactory {
    /// Factory serving `runtime`: view ids come from its allocator, and
    /// dropped view handles close their scripted views.
    #[must_use]
    pub fn new(runtime: &Arc<Runtime>, script: WebScript) -> Self {
        let hook_script = script.clone();
        let hook_runtime = Arc::downgrade(runtime);
        runtime.install_native_handle_release_hook(Arc::new(move |id| {
            if !id.is_rust_allocated() {
                return;
            }
            if let Some(view) = hook_script.state.views.release(id) {
                if let Some(rt) = hook_runtime.upgrade() {
                    unregister_frame_sink(&rt, id);
                }
                view.control.close();
                view.telemetry.close();
                hook_script.record(RecordedCall::Released(id));
            }
        }));
        Self {
            script,
            runtime: Arc::downgrade(runtime),
        }
    }
}

impl WebViewFactory for ScriptedWebViewFactory {
    type Instance = ScriptedWebView;

    fn create(&self, config: WebViewInitConfig) -> ScriptedWebView {
        ScriptedWebView {
            script: self.script.clone(),
            runtime: self.runtime.clone(),
            config,
        }
    }
}

/// The scripted host instance.
#[derive(Debug)]
pub struct ScriptedWebView {
    script: WebScript,
    runtime: Weak<Runtime>,
    config: WebViewInitConfig,
}

/// Records frames sent through the presenter.
struct RecordingSink {
    script: WebScript,
    view: NativeHandleId,
}

impl FrameSink for RecordingSink {
    fn set_frame(&self, frame: ViewFrame) {
        self.script.record(RecordedCall::Frame(self.view, frame));
    }
}

impl ScriptedWebView {
    fn view(&self, r: &ViewRef) -> Result<Arc<ScriptedView>, WebViewError> {
        self.script.state.views.get(r)
    }

    fn record(&self, call: RecordedCall) {
        self.script.record(call);
    }
}

impl WebView for ScriptedWebView {
    async fn capabilities(&self) -> Result<WebViewCapabilities, WebViewError> {
        Ok(WebViewCapabilities {
            backend: "scripted".into(),
            embedded: true,
            windowed: true,
            ephemeral_profiles: true,
            persistent_profiles: true,
            bridge_reply: true,
            deferred_navigation: true,
            inspector: self.config.allow_inspector,
            ..WebViewCapabilities::default()
        })
    }

    async fn prewarm(&self) -> Result<(), WebViewError> {
        Ok(())
    }

    async fn open(&self, spec: WebViewSpec) -> Result<WebViewInstance, WebViewError> {
        let rules = CompiledRules::new(&spec.rules)?;
        let runtime = self
            .runtime
            .upgrade()
            .ok_or_else(|| WebViewError::Backend("runtime is gone".into()))?;
        let id = runtime.alloc_native_handle_id();
        let view = Arc::new(ScriptedView {
            control: ControlLane::default(),
            telemetry: TelemetryLane::default(),
            rules,
            file_access: spec.settings.file_access,
            history: Mutex::default(),
            asks: PendingTable::default(),
            next_navigation: std::sync::atomic::AtomicU64::new(1),
        });
        let instance = self.script.state.views.insert(id, view);
        register_frame_sink(
            &runtime,
            id,
            Arc::new(RecordingSink {
                script: self.script.clone(),
                view: id,
            }),
        );
        self.record(RecordedCall::Open(spec));
        Ok(instance)
    }

    async fn close(&self, view: ViewRef) -> Result<(), WebViewError> {
        let removed = self.script.state.views.remove(&view)?;
        if let Some(rt) = self.runtime.upgrade() {
            unregister_frame_sink(&rt, view.view);
        }
        removed.control.close();
        removed.telemetry.close();
        self.record(RecordedCall::Close(view.view));
        Ok(())
    }

    async fn set_frame(&self, view: ViewRef, frame: ViewFrame) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::Frame(view.view, frame));
        Ok(())
    }

    async fn focus(&self, view: ViewRef) -> Result<(), WebViewError> {
        self.view(&view).map(drop)
    }

    async fn load(&self, view: ViewRef, request: LoadRequest) -> Result<(), WebViewError> {
        let target = self.view(&view)?;
        self.record(RecordedCall::Load(request.url.clone()));
        target.navigate(&request.url);
        Ok(())
    }

    async fn load_html(
        &self,
        view: ViewRef,
        html: String,
        base_url: Option<String>,
    ) -> Result<(), WebViewError> {
        let target = self.view(&view)?;
        self.record(RecordedCall::LoadHtml(html));
        target.navigate(base_url.as_deref().unwrap_or("about:blank"));
        Ok(())
    }

    async fn navigate_history(&self, view: ViewRef, nav: HistoryNav) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::History(nav));
        Ok(())
    }

    async fn history(&self, view: ViewRef) -> Result<WebHistory, WebViewError> {
        Ok(lock(&self.view(&view)?.history).clone())
    }

    async fn eval(&self, view: ViewRef, call: ScriptCall) -> Result<String, WebViewError> {
        self.view(&view)?;
        let result = lock(&self.script.state.eval)
            .as_ref()
            .map_or_else(|| Ok("null".to_owned()), |handler| handler(&call));
        self.record(RecordedCall::Eval(call));
        result
    }

    async fn post_message(
        &self,
        view: ViewRef,
        message: OutgoingMessage,
    ) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::PostMessage(message));
        Ok(())
    }

    async fn reply(&self, view: ViewRef, reply: BridgeReply) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::Reply(reply));
        Ok(())
    }

    async fn decide(&self, view: ViewRef, decision: Decision) -> Result<(), WebViewError> {
        let target = self.view(&view)?;
        self.record(RecordedCall::Decide(decision.clone()));
        // Navigation asks resolve here; every other kind is only recorded.
        if let Some(url) = target.asks.take(decision.request_id)
            && decision.response == DecisionResponse::Allow
        {
            let navigation_id = target.navigation_id();
            target.commit(NavInfo {
                navigation_id,
                url,
                is_redirect: false,
                user_gesture: false,
            });
        }
        Ok(())
    }

    async fn scheme_head(
        &self,
        view: ViewRef,
        head: SchemeResponseHead,
    ) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::SchemeHead(head));
        Ok(())
    }

    async fn scheme_chunk(&self, view: ViewRef, chunk: SchemeChunk) -> Result<(), WebViewError> {
        self.view(&view)?;
        self.record(RecordedCall::SchemeChunk(chunk));
        Ok(())
    }

    fn scheme_request_body(
        &self,
        view: ViewRef,
        _request_id: u64,
    ) -> flume::Receiver<Result<Vec<u8>, WebViewError>> {
        let (tx, rx) = flume::bounded(1);
        if let Err(err) = self.view(&view) {
            drop(tx.send(Err(err)));
        }
        rx
    }

    async fn cookies(
        &self,
        view: ViewRef,
        filter: CookieFilter,
    ) -> Result<Vec<WebCookie>, WebViewError> {
        self.view(&view)?;
        Ok(lock(&self.script.state.cookies)
            .iter()
            .filter(|c| filter.name.as_ref().is_none_or(|name| *name == c.name))
            .cloned()
            .collect())
    }

    async fn set_cookie(&self, view: ViewRef, cookie: WebCookie) -> Result<(), WebViewError> {
        self.view(&view)?;
        lock(&self.script.state.cookies).push(cookie.clone());
        self.record(RecordedCall::SetCookie(cookie));
        Ok(())
    }

    async fn clear_data(
        &self,
        view: ViewRef,
        request: ClearDataRequest,
    ) -> Result<(), WebViewError> {
        self.view(&view)?;
        lock(&self.script.state.cookies).clear();
        self.record(RecordedCall::ClearData(request));
        Ok(())
    }

    async fn set_zoom(&self, view: ViewRef, _factor: f32) -> Result<(), WebViewError> {
        self.view(&view).map(drop)
    }

    async fn find(&self, view: ViewRef, _query: FindQuery) -> Result<FindResult, WebViewError> {
        self.view(&view)?;
        Ok(FindResult::default())
    }

    async fn snapshot(
        &self,
        view: ViewRef,
        _options: SnapshotOptions,
    ) -> Result<WebSnapshot, WebViewError> {
        self.view(&view)?;
        Err(WebViewError::Unsupported("snapshot".into()))
    }

    async fn print(&self, view: ViewRef) -> Result<(), WebViewError> {
        self.view(&view).map(drop)
    }

    async fn set_inspector(&self, view: ViewRef, _open: bool) -> Result<(), WebViewError> {
        self.view(&view)?;
        if self.config.allow_inspector {
            Ok(())
        } else {
            Err(WebViewError::Unsupported("inspector disabled".into()))
        }
    }

    async fn download_control(&self, view: ViewRef, _op: DownloadOp) -> Result<(), WebViewError> {
        self.view(&view)?;
        Err(WebViewError::UnknownRequest)
    }

    fn control_events(&self, view: ViewRef) -> flume::Receiver<Result<ControlEvent, WebViewError>> {
        match self.view(&view).and_then(|v| v.control.subscribe()) {
            Ok(rx) => rx,
            Err(err) => {
                let (tx, rx) = flume::bounded(1);
                drop(tx.send(Err(err)));
                rx
            }
        }
    }

    fn telemetry(
        &self,
        view: ViewRef,
        kinds: Vec<TelemetryKind>,
    ) -> flume::Receiver<Result<TelemetryEvent, WebViewError>> {
        match self.view(&view) {
            Ok(v) => v.telemetry.subscribe(kinds),
            Err(err) => {
                let (tx, rx) = flume::bounded(1);
                drop(tx.send(Err(err)));
                rx
            }
        }
    }
}
