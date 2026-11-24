//! Ergonomic client surface: [`ViewHandle`] and the request objects
//! whose drop answers with the safe default.

use std::sync::Arc;

use istmo_core::sync::lock;
use istmo_core::{IstmoError, NativeHandle, NativeHandleId, StreamItem, TypedStream, codec};

use crate::{
    BridgeConfig, BridgeMessage, BridgeReply, ClearDataRequest, ControlEvent, CookieFilter,
    Decision, DecisionKind, DecisionRequest, DecisionResponse, DecisionTimeouts, EmbeddedTarget,
    FindQuery, FindResult, HistoryNav, InjectionTime, LoadRequest, NavigationRule, NavigationRules,
    OutgoingMessage, Presenter, RenderTarget, ReplyResult, RuleAction, SchemeBody, SchemeChunk,
    SchemeRequestInfo, SchemeResponseHead, ScriptCall, ScriptWorld, SnapshotOptions,
    TelemetryEvent, TelemetryKind, UrlPattern, UserScript, ViewRef, WEBVIEW_PLUGIN_ID, WebCookie,
    WebHistory, WebProfile, WebSnapshot, WebViewClient, WebViewError, WebViewInstance,
    WebViewSettings, WebViewSpec, WebViewTag, WindowTarget,
};

// -------------------------------------------------------------------
// Spec builders
// -------------------------------------------------------------------

impl WebViewSpec {
    /// Embedded view in the registered window `window_id`, with the
    /// locked-down defaults: shared profile, allow-all rules, no
    /// bridge, no telemetry.
    #[must_use]
    pub fn embedded(window_id: u64) -> Self {
        Self::new(RenderTarget::Embedded(EmbeddedTarget { window_id }))
    }

    /// Plugin-owned window.
    #[must_use]
    pub fn window(title: impl Into<String>, width: u32, height: u32) -> Self {
        Self::new(RenderTarget::Window(WindowTarget {
            title: title.into(),
            width,
            height,
            parent_window_id: None,
            modal: false,
            resizable: true,
        }))
    }

    fn new(target: RenderTarget) -> Self {
        Self {
            target,
            profile: WebProfile::Shared,
            settings: WebViewSettings::default(),
            rules: NavigationRules::default(),
            user_scripts: Vec::new(),
            bridge: None,
            timeouts: DecisionTimeouts::default(),
            telemetry: Vec::new(),
        }
    }

    /// Storage profile.
    #[must_use]
    pub fn with_profile(mut self, profile: WebProfile) -> Self {
        self.profile = profile;
        self
    }

    /// Engine settings.
    #[must_use]
    pub fn with_settings(mut self, settings: WebViewSettings) -> Self {
        self.settings = settings;
        self
    }

    /// Navigation rules.
    #[must_use]
    pub fn with_rules(mut self, rules: NavigationRules) -> Self {
        self.rules = rules;
        self
    }

    /// Add a user script.
    #[must_use]
    pub fn with_script(mut self, script: UserScript) -> Self {
        self.user_scripts.push(script);
        self
    }

    /// Enable the JS bridge.
    #[must_use]
    pub fn with_bridge(mut self, bridge: BridgeConfig) -> Self {
        self.bridge = Some(bridge);
        self
    }

    /// Decision timeouts.
    #[must_use]
    pub const fn with_timeouts(mut self, timeouts: DecisionTimeouts) -> Self {
        self.timeouts = timeouts;
        self
    }

    /// Produce these telemetry kinds.
    #[must_use]
    pub fn with_telemetry(mut self, kinds: impl IntoIterator<Item = TelemetryKind>) -> Self {
        self.telemetry.extend(kinds);
        self
    }
}

/// Sugar kept as a trait so the generated wire type stays plain data.
pub trait WebViewSpecExt {
    /// Settings for a sign-in view: ephemeral profile, no autofill,
    /// deny everything but `allowed` hosts, intercept `redirect`.
    #[must_use]
    fn sign_in(self, redirect: UrlPattern, allowed: impl IntoIterator<Item = UrlPattern>) -> Self;
}

impl WebViewSpecExt for WebViewSpec {
    fn sign_in(
        mut self,
        redirect: UrlPattern,
        allowed: impl IntoIterator<Item = UrlPattern>,
    ) -> Self {
        self.profile = WebProfile::Ephemeral;
        self.settings.autofill = false;
        let mut rules = vec![NavigationRule {
            pattern: redirect,
            action: RuleAction::Intercept,
        }];
        rules.extend(allowed.into_iter().map(|pattern| NavigationRule {
            pattern,
            action: RuleAction::Allow,
        }));
        self.rules = NavigationRules {
            default_action: RuleAction::Deny,
            rules,
        };
        self
    }
}

impl UrlPattern {
    /// `scheme://host` on the default port, any path.
    #[must_use]
    pub fn host(scheme: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            scheme: scheme.into(),
            host: host.into(),
            port: None,
            path_prefix: String::new(),
        }
    }

    /// Match this port instead of the default one.
    #[must_use]
    pub const fn with_port(mut self, port: u16) -> Self {
        self.port = Some(port);
        self
    }

    /// Match paths under `prefix`.
    #[must_use]
    pub fn with_path(mut self, prefix: impl Into<String>) -> Self {
        self.path_prefix = prefix.into();
        self
    }
}

impl NavigationRules {
    /// Deny by default, allow the given patterns.
    #[must_use]
    pub fn allow_only(patterns: impl IntoIterator<Item = UrlPattern>) -> Self {
        Self {
            default_action: RuleAction::Deny,
            rules: patterns
                .into_iter()
                .map(|pattern| NavigationRule {
                    pattern,
                    action: RuleAction::Allow,
                })
                .collect(),
        }
    }

    /// Add a rule after the existing ones.
    #[must_use]
    pub fn with(mut self, pattern: UrlPattern, action: RuleAction) -> Self {
        self.rules.push(NavigationRule { pattern, action });
        self
    }
}

impl BridgeConfig {
    /// Bridge with `channels`, open to no origin yet.
    #[must_use]
    pub fn new(channels: impl IntoIterator<Item = impl Into<String>>) -> Self {
        Self {
            channels: channels.into_iter().map(Into::into).collect(),
            allowed_origins: Vec::new(),
            allow_subframes: false,
        }
    }

    /// Open the bridge to `origin`.
    #[must_use]
    pub fn allow_origin(mut self, origin: impl Into<String>) -> Self {
        self.allowed_origins.push(origin.into());
        self
    }
}

impl UserScript {
    /// Main-frame script at document start in the page world, for every
    /// origin.
    #[must_use]
    pub fn at_document_start(source: impl Into<String>) -> Self {
        Self {
            source: source.into(),
            inject_at: InjectionTime::DocumentStart,
            main_frame_only: true,
            world: ScriptWorld::Page,
            allowed_origins: Vec::new(),
        }
    }
}

// -------------------------------------------------------------------
// ViewHandle
// -------------------------------------------------------------------

/// Owned view. Dropping it closes the view.
#[derive(Debug)]
pub struct ViewHandle {
    client: Arc<WebViewClient>,
    view: ViewRef,
    presenter: Presenter,
    control: std::sync::Mutex<Option<TypedStream<ControlEvent, WebViewError>>>,
    // Last: dropping it sends the release that closes the native view.
    handle: NativeHandle<WebViewTag>,
}

/// Extension trait giving [`WebViewClient`] view helpers.
pub trait WebViewClientExt {
    /// Open a view and subscribe to its control lane before anything
    /// can be missed.
    ///
    /// # Errors
    /// Propagates [`WebViewError`] from the backend.
    fn open_view(
        &self,
        spec: WebViewSpec,
    ) -> impl std::future::Future<Output = Result<ViewHandle, WebViewError>> + Send;

    /// Take ownership of a view the page opened
    /// ([`ControlEvent::NewView`]).
    ///
    /// # Errors
    /// [`WebViewError`] when its control lane cannot be subscribed.
    fn adopt(&self, instance: WebViewInstance) -> Result<ViewHandle, WebViewError>;
}

impl WebViewClientExt for Arc<WebViewClient> {
    async fn open_view(&self, spec: WebViewSpec) -> Result<ViewHandle, WebViewError> {
        let instance = self.open(spec).await.map_err(WebViewError::from)?;
        self.adopt(instance)
    }

    fn adopt(&self, instance: WebViewInstance) -> Result<ViewHandle, WebViewError> {
        let view = crate::view_ref(&instance);
        let owned = instance.into_owned(self.runtime());
        let control = self.control_events(view).map_err(WebViewError::from)?;
        Ok(ViewHandle {
            presenter: Presenter::new(view, Arc::clone(self.runtime()), self.instance_id()),
            client: Self::clone(self),
            view,
            control: std::sync::Mutex::new(Some(control)),
            handle: owned.view,
        })
    }
}

macro_rules! forward {
    ($(#[$doc:meta])* $name:ident($($arg:ident: $ty:ty),*) -> $ret:ty) => {
        $(#[$doc])*
        ///
        /// # Errors
        /// Propagates [`WebViewError`] from the backend.
        pub async fn $name(&self, $($arg: $ty),*) -> Result<$ret, WebViewError> {
            self.client
                .$name(self.view, $($arg),*)
                .await
                .map_err(WebViewError::from)
        }
    };
}

impl ViewHandle {
    /// Handle id of the view.
    #[must_use]
    pub const fn id(&self) -> NativeHandleId {
        self.handle.id()
    }

    /// View reference for calls on the raw client.
    #[must_use]
    pub const fn view_ref(&self) -> ViewRef {
        self.view
    }

    /// Per-frame placement of an embedded view.
    #[must_use]
    pub const fn presenter(&self) -> &Presenter {
        &self.presenter
    }

    /// Typed control events. The lane has one subscriber: the first call
    /// takes it, later calls get `None`.
    #[must_use]
    pub fn control(&self) -> Option<ViewEvents> {
        lock(&self.control).take().map(|stream| ViewEvents {
            stream,
            client: Arc::clone(&self.client),
            view: self.view,
        })
    }

    /// Telemetry for `kinds` (only those the view was opened with).
    ///
    /// # Errors
    /// Transport failures as [`WebViewError::Backend`].
    pub fn telemetry(
        &self,
        kinds: impl IntoIterator<Item = TelemetryKind>,
    ) -> Result<TypedStream<TelemetryEvent, WebViewError>, WebViewError> {
        self.client
            .telemetry(self.view, kinds.into_iter().collect())
            .map_err(WebViewError::from)
    }

    /// `GET url`.
    ///
    /// # Errors
    /// Propagates [`WebViewError`] from the backend.
    pub async fn load_url(&self, url: impl Into<String>) -> Result<(), WebViewError> {
        self.load(LoadRequest::get(url)).await
    }

    forward!(
        /// Navigate the main frame.
        load(request: LoadRequest) -> ()
    );
    forward!(
        /// Show `html` with `base_url` as its URL.
        load_html(html: String, base_url: Option<String>) -> ()
    );
    forward!(
        /// Back, forward, reload, stop.
        navigate_history(nav: HistoryNav) -> ()
    );
    forward!(
        /// The back/forward list.
        history() -> WebHistory
    );
    forward!(
        /// Keyboard focus.
        focus() -> ()
    );
    forward!(
        /// Evaluate a script; JSON of the result.
        eval(call: ScriptCall) -> String
    );
    forward!(
        /// Bridge message to the page.
        post_message(message: OutgoingMessage) -> ()
    );
    forward!(
        /// Cookies of the view's profile.
        cookies(filter: CookieFilter) -> Vec<WebCookie>
    );
    forward!(
        /// Store a cookie.
        set_cookie(cookie: WebCookie) -> ()
    );
    forward!(
        /// Remove stored data.
        clear_data(request: ClearDataRequest) -> ()
    );
    forward!(
        /// Page zoom.
        set_zoom(factor: f32) -> ()
    );
    forward!(
        /// Find in page.
        find(query: FindQuery) -> FindResult
    );
    forward!(
        /// Encode the viewport.
        snapshot(options: SnapshotOptions) -> WebSnapshot
    );
    forward!(
        /// Print.
        print() -> ()
    );
    forward!(
        /// Inspector.
        set_inspector(open: bool) -> ()
    );

    /// Close the view and report the backend's answer. Dropping the
    /// handle closes it too, without a result.
    ///
    /// # Errors
    /// Propagates [`WebViewError`] from the backend; the handle is then
    /// dropped normally, so the release path still runs.
    pub async fn close(self) -> Result<(), WebViewError> {
        self.client
            .close(self.view)
            .await
            .map_err(WebViewError::from)?;
        let _: NativeHandleId = self.handle.into_id();
        Ok(())
    }
}

// -------------------------------------------------------------------
// Typed events
// -------------------------------------------------------------------

/// The control lane of one view with requests wrapped in objects that
/// answer safely when dropped.
#[derive(Debug)]
pub struct ViewEvents {
    stream: TypedStream<ControlEvent, WebViewError>,
    client: Arc<WebViewClient>,
    view: ViewRef,
}

/// One control event.
#[derive(Debug)]
pub enum ViewEvent {
    /// Answer it; dropping it denies.
    Decision(PendingDecision),
    /// Answer it; dropping it fails the request.
    Scheme(PendingSchemeRequest),
    /// `istmo.invoke`: answer it; dropping it rejects the promise.
    Invoke(BridgeRequest),
    /// Everything else, including fire-and-forget bridge messages.
    Other(ControlEvent),
}

impl ViewEvents {
    /// Next event; `None` once the view is closed.
    pub async fn next(&self) -> Option<ViewEvent> {
        match self.stream.recv_async().await {
            Ok(StreamItem::Event(event)) => Some(self.wrap(event)),
            Ok(StreamItem::Failed(err)) => {
                tracing::debug!(%err, "web view control lane failed");
                None
            }
            Ok(StreamItem::Completed | StreamItem::Cancelled) | Err(_) => None,
        }
    }

    /// Blocking [`Self::next`].
    #[must_use]
    pub fn next_blocking(&self) -> Option<ViewEvent> {
        match self.stream.recv() {
            Ok(StreamItem::Event(event)) => Some(self.wrap(event)),
            _ => None,
        }
    }

    fn wrap(&self, event: ControlEvent) -> ViewEvent {
        let answer = || Answer {
            client: Arc::clone(&self.client),
            view: self.view,
        };
        match event {
            ControlEvent::DecisionRequested(request) => ViewEvent::Decision(PendingDecision {
                answer: answer(),
                request,
                answered: false,
            }),
            ControlEvent::SchemeRequest(info) => ViewEvent::Scheme(PendingSchemeRequest {
                answer: answer(),
                info,
                finished: false,
            }),
            ControlEvent::Message(message) if message.reply_id.is_some() => {
                ViewEvent::Invoke(BridgeRequest {
                    answer: answer(),
                    message,
                    answered: false,
                })
            }
            other => ViewEvent::Other(other),
        }
    }
}

/// What request objects need to answer.
#[derive(Debug)]
struct Answer {
    client: Arc<WebViewClient>,
    view: ViewRef,
}

impl Answer {
    /// Fire-and-forget answer from a `Drop`: never blocks.
    fn notify<T: istmo_core::codec::Encode>(&self, method: &str, arg: T) {
        let payload = match codec::encode(&(self.view, arg)) {
            Ok(payload) => payload,
            Err(err) => {
                tracing::error!(%err, method, "encode web view answer");
                return;
            }
        };
        if let Err(err) = self.client.runtime().notify(
            WEBVIEW_PLUGIN_ID,
            self.client.instance_id(),
            method,
            payload,
        ) {
            tracing::debug!(%err, method, "web view answer not delivered");
        }
    }
}

fn lift(err: IstmoError) -> WebViewError {
    WebViewError::from(err)
}

/// A decision the page or a rule is waiting for.
#[derive(Debug)]
pub struct PendingDecision {
    answer: Answer,
    request: DecisionRequest,
    answered: bool,
}

impl PendingDecision {
    /// What is being asked.
    #[must_use]
    pub const fn kind(&self) -> &DecisionKind {
        &self.request.kind
    }

    /// Answer with `response`.
    ///
    /// # Errors
    /// [`WebViewError::UnknownRequest`] when it already timed out.
    pub async fn respond(mut self, response: DecisionResponse) -> Result<(), WebViewError> {
        self.answered = true;
        let decision = Decision {
            request_id: self.request.request_id,
            response,
        };
        self.answer
            .client
            .decide(self.answer.view, decision)
            .await
            .map_err(lift)
    }

    /// [`DecisionResponse::Allow`].
    ///
    /// # Errors
    /// As [`Self::respond`].
    pub async fn allow(self) -> Result<(), WebViewError> {
        self.respond(DecisionResponse::Allow).await
    }

    /// [`DecisionResponse::Deny`].
    ///
    /// # Errors
    /// As [`Self::respond`].
    pub async fn deny(self) -> Result<(), WebViewError> {
        self.respond(DecisionResponse::Deny).await
    }
}

impl Drop for PendingDecision {
    fn drop(&mut self) {
        if !self.answered {
            self.answer.notify(
                "decide",
                Decision {
                    request_id: self.request.request_id,
                    response: DecisionResponse::Deny,
                },
            );
        }
    }
}

/// A request to a dynamic custom scheme.
#[derive(Debug)]
pub struct PendingSchemeRequest {
    answer: Answer,
    info: SchemeRequestInfo,
    finished: bool,
}

impl PendingSchemeRequest {
    /// The request.
    #[must_use]
    pub const fn info(&self) -> &SchemeRequestInfo {
        &self.info
    }

    /// Answer with a whole body.
    ///
    /// # Errors
    /// [`WebViewError::UnknownRequest`] when the request timed out.
    pub async fn respond(
        mut self,
        status: u16,
        mime_type: impl Into<String>,
        body: Vec<u8>,
    ) -> Result<(), WebViewError> {
        let length = u64::try_from(body.len()).ok();
        self.head(status, mime_type.into(), length).await?;
        self.chunk(SchemeBody::Data(body)).await?;
        self.finished = true;
        self.chunk(SchemeBody::End).await
    }

    /// Start a streamed answer; send the body with
    /// [`SchemeResponseWriter::write`].
    ///
    /// # Errors
    /// [`WebViewError::UnknownRequest`] when the request timed out.
    pub async fn stream(
        mut self,
        status: u16,
        mime_type: impl Into<String>,
        content_length: Option<u64>,
    ) -> Result<SchemeResponseWriter, WebViewError> {
        self.head(status, mime_type.into(), content_length).await?;
        self.finished = true;
        Ok(SchemeResponseWriter {
            answer: Answer {
                client: Arc::clone(&self.answer.client),
                view: self.answer.view,
            },
            request_id: self.info.request_id,
            ended: false,
        })
    }

    async fn head(
        &self,
        status: u16,
        mime_type: String,
        content_length: Option<u64>,
    ) -> Result<(), WebViewError> {
        let head = SchemeResponseHead {
            request_id: self.info.request_id,
            status,
            mime_type,
            headers: Vec::new(),
            content_length,
        };
        self.answer
            .client
            .scheme_head(self.answer.view, head)
            .await
            .map_err(lift)
    }

    async fn chunk(&self, body: SchemeBody) -> Result<(), WebViewError> {
        let chunk = SchemeChunk {
            request_id: self.info.request_id,
            body,
        };
        self.answer
            .client
            .scheme_chunk(self.answer.view, chunk)
            .await
            .map_err(lift)
    }
}

impl Drop for PendingSchemeRequest {
    fn drop(&mut self) {
        if !self.finished {
            self.answer.notify(
                "scheme_chunk",
                SchemeChunk {
                    request_id: self.info.request_id,
                    body: SchemeBody::Abort("not handled".into()),
                },
            );
        }
    }
}

/// Body writer of a streamed scheme response.
#[derive(Debug)]
pub struct SchemeResponseWriter {
    answer: Answer,
    request_id: u64,
    ended: bool,
}

impl SchemeResponseWriter {
    /// Send the next piece of the body.
    ///
    /// # Errors
    /// [`WebViewError::UnknownRequest`] when the page went away.
    pub async fn write(&self, data: Vec<u8>) -> Result<(), WebViewError> {
        self.send(SchemeBody::Data(data)).await
    }

    /// End the body.
    ///
    /// # Errors
    /// As [`Self::write`].
    pub async fn finish(mut self) -> Result<(), WebViewError> {
        self.ended = true;
        self.send(SchemeBody::End).await
    }

    async fn send(&self, body: SchemeBody) -> Result<(), WebViewError> {
        let chunk = SchemeChunk {
            request_id: self.request_id,
            body,
        };
        self.answer
            .client
            .scheme_chunk(self.answer.view, chunk)
            .await
            .map_err(lift)
    }
}

impl Drop for SchemeResponseWriter {
    fn drop(&mut self) {
        if !self.ended {
            self.answer.notify(
                "scheme_chunk",
                SchemeChunk {
                    request_id: self.request_id,
                    body: SchemeBody::Abort("response dropped".into()),
                },
            );
        }
    }
}

/// An `istmo.invoke` call waiting for its result.
#[derive(Debug)]
pub struct BridgeRequest {
    answer: Answer,
    message: BridgeMessage,
    answered: bool,
}

impl BridgeRequest {
    /// The page's message.
    #[must_use]
    pub const fn message(&self) -> &BridgeMessage {
        &self.message
    }

    /// Resolve (`Ok`) or reject (`Err`) the page's promise.
    ///
    /// # Errors
    /// [`WebViewError::UnknownRequest`] when the document is gone.
    pub async fn reply(mut self, result: Result<Vec<u8>, String>) -> Result<(), WebViewError> {
        self.answered = true;
        let reply = self.reply_for(result);
        self.answer
            .client
            .reply(self.answer.view, reply)
            .await
            .map_err(lift)
    }

    fn reply_for(&self, result: Result<Vec<u8>, String>) -> BridgeReply {
        BridgeReply {
            reply_id: self.message.reply_id.unwrap_or_default(),
            result: match result {
                Ok(data) => ReplyResult::Ok(data),
                Err(message) => ReplyResult::Err(message),
            },
        }
    }
}

impl Drop for BridgeRequest {
    fn drop(&mut self) {
        if !self.answered {
            let reply = self.reply_for(Err("not handled".into()));
            self.answer.notify("reply", reply);
        }
    }
}
