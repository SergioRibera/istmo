//! [`DesktopWebView`]: the [`WebView`] host over a desktop [`Engine`].

use std::sync::{Arc, Weak};

use istmo_core::Runtime;

use super::core::{Done, Engine, PopupMaker, UiView, ViewCore};
use crate::host::ViewTable;
use crate::present::{register_frame_sink, unregister_frame_sink};
use crate::{
    BridgeReply, ClearDataRequest, ControlEvent, CookieFilter, Decision, DownloadOp, FindQuery,
    FindResult, HistoryNav, LoadRequest, OutgoingMessage, SchemeChunk, SchemeResponseHead,
    ScriptCall, SnapshotOptions, TelemetryEvent, TelemetryKind, ViewFrame, ViewRef, WebCookie,
    WebHistory, WebSnapshot, WebView, WebViewCapabilities, WebViewError, WebViewInitConfig,
    WebViewInstance, WebViewSpec,
};

/// One client instance's host: views of one engine.
pub struct DesktopWebView {
    config: Arc<WebViewInitConfig>,
    engine: Result<Arc<dyn Engine>, WebViewError>,
    views: Arc<ViewTable<ViewCore>>,
    runtime: Weak<Runtime>,
}

impl std::fmt::Debug for DesktopWebView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DesktopWebView").finish_non_exhaustive()
    }
}

/// Block on an engine call from the host's call thread.
async fn wait<T: Send + 'static>(start: impl FnOnce(Done<T>)) -> Result<T, WebViewError> {
    let (tx, rx) = oneshot::channel();
    start(Box::new(move |result| drop(tx.send(result))));
    rx.await
        .unwrap_or_else(|_| Err(WebViewError::Backend("engine dropped the call".into())))
}

impl DesktopWebView {
    pub(crate) fn new(
        runtime: &Arc<Runtime>,
        config: WebViewInitConfig,
        engine: Result<Arc<dyn Engine>, WebViewError>,
    ) -> Self {
        let views: Arc<ViewTable<ViewCore>> = Arc::default();
        install_release_hook(runtime, &views, engine.as_ref().ok());
        Self {
            config: Arc::new(config),
            engine,
            views,
            runtime: Arc::downgrade(runtime),
        }
    }

    /// Host whose runtime is gone: every call fails.
    pub(crate) fn detached(config: WebViewInitConfig) -> Self {
        Self {
            config: Arc::new(config),
            engine: Err(WebViewError::Backend("istmo runtime is gone".into())),
            views: Arc::default(),
            runtime: Weak::new(),
        }
    }

    fn engine(&self) -> Result<&Arc<dyn Engine>, WebViewError> {
        self.engine.as_ref().map_err(Clone::clone)
    }

    fn runtime(&self) -> Result<Arc<Runtime>, WebViewError> {
        self.runtime
            .upgrade()
            .ok_or_else(|| WebViewError::Backend("istmo runtime is gone".into()))
    }

    fn popup_maker(&self, engine: &Arc<dyn Engine>) -> Arc<dyn PopupMaker> {
        Arc::new(Popups {
            runtime: self.runtime.clone(),
            engine: Arc::downgrade(engine),
            views: Arc::downgrade(&self.views),
            config: Arc::clone(&self.config),
        })
    }

    /// Run `f` on the view's native object.
    async fn on_view<T: Send + 'static>(
        &self,
        view: &ViewRef,
        f: impl FnOnce(&mut dyn UiView, Done<T>) + Send + 'static,
    ) -> Result<T, WebViewError> {
        self.views.get(view)?;
        let engine = Arc::clone(self.engine()?);
        let id = view.view;
        wait(move |done| {
            engine.with_view(
                id,
                Box::new(move |native| match native {
                    Some(native) => f(native, done),
                    None => done(Err(WebViewError::ViewClosed)),
                }),
            );
        })
        .await
    }

    /// [`Self::on_view`] for synchronous engine calls.
    async fn sync_view<T: Send + 'static>(
        &self,
        view: &ViewRef,
        f: impl FnOnce(&mut dyn UiView) -> Result<T, WebViewError> + Send + 'static,
    ) -> Result<T, WebViewError> {
        self.on_view(view, move |native, done| done(f(native)))
            .await
    }
}

/// A dropped `ViewHandle` sends a release; close the view it named.
fn install_release_hook(
    runtime: &Arc<Runtime>,
    views: &Arc<ViewTable<ViewCore>>,
    engine: Option<&Arc<dyn Engine>>,
) {
    let views = Arc::downgrade(views);
    let engine = engine.map(Arc::downgrade);
    let hook_runtime = Arc::downgrade(runtime);
    runtime.install_native_handle_release_hook(Arc::new(move |id| {
        if !id.is_rust_allocated() {
            return;
        }
        let (Some(views), Some(engine)) =
            (views.upgrade(), engine.as_ref().and_then(Weak::upgrade))
        else {
            return;
        };
        if let Some(core) = views.release(id) {
            if let Some(rt) = hook_runtime.upgrade() {
                unregister_frame_sink(&rt, id);
            }
            teardown(&engine, &core);
        }
    }));
}

struct Popups {
    runtime: Weak<Runtime>,
    engine: Weak<dyn Engine>,
    views: Weak<ViewTable<ViewCore>>,
    config: Arc<WebViewInitConfig>,
}

impl PopupMaker for Popups {
    fn make(&self, spec: WebViewSpec) -> Option<(Arc<ViewCore>, WebViewInstance)> {
        let (runtime, engine, views) = (
            self.runtime.upgrade()?,
            self.engine.upgrade()?,
            self.views.upgrade()?,
        );
        let id = runtime.alloc_native_handle_id();
        let core = Arc::new(
            ViewCore::new(id, spec, Arc::clone(&self.config), Arc::downgrade(&engine)).ok()?,
        );
        core.set_popup_maker(Arc::new(Self {
            runtime: self.runtime.clone(),
            engine: self.engine.clone(),
            views: self.views.clone(),
            config: Arc::clone(&self.config),
        }));
        register_frame_sink(&runtime, id, engine.frame_sink(id));
        let instance = views.insert(id, Arc::clone(&core));
        Some((core, instance))
    }
}

fn teardown(engine: &Arc<dyn Engine>, core: &Arc<ViewCore>) {
    core.close();
    engine.with_view(
        core.id,
        Box::new(|native| {
            if let Some(native) = native {
                native.close();
            }
        }),
    );
}

fn failed_stream<T>(err: WebViewError) -> flume::Receiver<Result<T, WebViewError>> {
    let (tx, rx) = flume::bounded(1);
    drop(tx.send(Err(err)));
    rx
}

impl WebView for DesktopWebView {
    async fn capabilities(&self) -> Result<WebViewCapabilities, WebViewError> {
        let engine = self.engine()?;
        let (backend, engine_version) = engine.describe();
        let mut caps = WebViewCapabilities {
            backend,
            engine_version,
            inspector: self.config.allow_inspector,
            ..WebViewCapabilities::default()
        };
        engine.capabilities(&mut caps);
        Ok(caps)
    }

    async fn prewarm(&self) -> Result<(), WebViewError> {
        self.engine().map(drop)
    }

    async fn open(&self, spec: WebViewSpec) -> Result<WebViewInstance, WebViewError> {
        let engine = Arc::clone(self.engine()?);
        let runtime = self.runtime()?;
        let id = runtime.alloc_native_handle_id();
        let core = Arc::new(ViewCore::new(
            id,
            spec,
            Arc::clone(&self.config),
            Arc::downgrade(&engine),
        )?);
        let creating = Arc::clone(&core);
        let engine_for_create = Arc::clone(&engine);
        wait(move |done| engine_for_create.create(creating, done)).await?;
        register_frame_sink(&runtime, id, engine.frame_sink(id));
        core.set_popup_maker(self.popup_maker(&engine));
        Ok(self.views.insert(id, core))
    }

    async fn close(&self, view: ViewRef) -> Result<(), WebViewError> {
        let core = self.views.remove(&view)?;
        if let Ok(rt) = self.runtime() {
            unregister_frame_sink(&rt, view.view);
        }
        teardown(self.engine()?, &core);
        Ok(())
    }

    async fn set_frame(&self, view: ViewRef, frame: ViewFrame) -> Result<(), WebViewError> {
        self.views.get(&view)?;
        self.engine()?.frame_sink(view.view).set_frame(frame);
        Ok(())
    }

    async fn focus(&self, view: ViewRef) -> Result<(), WebViewError> {
        self.sync_view(&view, |v| {
            v.focus();
            Ok(())
        })
        .await
    }

    async fn load(&self, view: ViewRef, request: LoadRequest) -> Result<(), WebViewError> {
        let core = self.views.get(&view)?;
        if !core.admit_load(&request.url) {
            return Ok(());
        }
        self.sync_view(&view, move |v| {
            v.load(&request);
            Ok(())
        })
        .await
    }

    async fn load_html(
        &self,
        view: ViewRef,
        html: String,
        base_url: Option<String>,
    ) -> Result<(), WebViewError> {
        let core = self.views.get(&view)?;
        if core.spec.bridge.is_some()
            && let Some(base) = &base_url
        {
            let origin = url::Url::parse(base)
                .ok()
                .as_ref()
                .and_then(crate::rules::origin_of)
                .ok_or_else(|| WebViewError::InvalidUrl(base.clone()))?;
            if !core.bridge_origins().contains(&origin) {
                return Err(WebViewError::InvalidArgument(
                    "base_url origin must be in the bridge's allowed_origins".into(),
                ));
            }
        }
        self.sync_view(&view, move |v| {
            v.load_html(&html, base_url.as_deref());
            Ok(())
        })
        .await
    }

    async fn navigate_history(&self, view: ViewRef, nav: HistoryNav) -> Result<(), WebViewError> {
        self.sync_view(&view, move |v| {
            v.navigate(nav);
            Ok(())
        })
        .await
    }

    async fn history(&self, view: ViewRef) -> Result<WebHistory, WebViewError> {
        self.sync_view(&view, |v| Ok(v.history())).await
    }

    async fn eval(&self, view: ViewRef, call: ScriptCall) -> Result<String, WebViewError> {
        let core = self.views.get(&view)?;
        if core.committed_origin().as_deref() != Some(call.expected_origin.as_str()) {
            return Err(WebViewError::OriginMismatch);
        }
        self.on_view(&view, move |v, done| v.eval(&call, done))
            .await
    }

    async fn post_message(
        &self,
        view: ViewRef,
        message: OutgoingMessage,
    ) -> Result<(), WebViewError> {
        let core = self.views.get(&view)?;
        let origin = core.committed_origin();
        if origin.as_deref() != Some(message.expected_origin.as_str())
            || !core.bridge_allows(&message.expected_origin, true)
        {
            return Err(WebViewError::OriginMismatch);
        }
        self.sync_view(&view, move |v| {
            v.deliver(&message.channel, &message.data);
            Ok(())
        })
        .await
    }

    async fn reply(&self, view: ViewRef, reply: BridgeReply) -> Result<(), WebViewError> {
        self.views.get(&view)?.reply(reply.reply_id, reply.result)
    }

    async fn decide(&self, view: ViewRef, decision: Decision) -> Result<(), WebViewError> {
        self.views
            .get(&view)?
            .decide(decision.request_id, decision.response)
    }

    async fn scheme_head(
        &self,
        view: ViewRef,
        head: SchemeResponseHead,
    ) -> Result<(), WebViewError> {
        self.views.get(&view)?.scheme_head(head)
    }

    async fn scheme_chunk(&self, view: ViewRef, chunk: SchemeChunk) -> Result<(), WebViewError> {
        self.views
            .get(&view)?
            .scheme_chunk(chunk.request_id, chunk.body)
    }

    fn scheme_request_body(
        &self,
        view: ViewRef,
        _request_id: u64,
    ) -> flume::Receiver<Result<Vec<u8>, WebViewError>> {
        failed_stream(match self.views.get(&view) {
            Ok(_) => WebViewError::Unsupported("request bodies on this engine".into()),
            Err(err) => err,
        })
    }

    async fn cookies(
        &self,
        view: ViewRef,
        filter: CookieFilter,
    ) -> Result<Vec<WebCookie>, WebViewError> {
        self.on_view(&view, move |v, done| v.cookies(&filter, done))
            .await
    }

    async fn set_cookie(&self, view: ViewRef, cookie: WebCookie) -> Result<(), WebViewError> {
        self.on_view(&view, move |v, done| v.set_cookie(&cookie, done))
            .await
    }

    async fn clear_data(
        &self,
        view: ViewRef,
        request: ClearDataRequest,
    ) -> Result<(), WebViewError> {
        self.on_view(&view, move |v, done| v.clear_data(&request, done))
            .await
    }

    async fn set_zoom(&self, view: ViewRef, factor: f32) -> Result<(), WebViewError> {
        if !(0.1..=10.0).contains(&factor) {
            return Err(WebViewError::InvalidArgument("zoom factor".into()));
        }
        self.sync_view(&view, move |v| {
            v.set_zoom(factor);
            Ok(())
        })
        .await
    }

    async fn find(&self, view: ViewRef, query: FindQuery) -> Result<FindResult, WebViewError> {
        self.on_view(&view, move |v, done| v.find(&query, done))
            .await
    }

    async fn snapshot(
        &self,
        view: ViewRef,
        options: SnapshotOptions,
    ) -> Result<WebSnapshot, WebViewError> {
        self.on_view(&view, move |v, done| v.snapshot(options, done))
            .await
    }

    async fn print(&self, view: ViewRef) -> Result<(), WebViewError> {
        self.sync_view(&view, |v| v.print()).await
    }

    async fn set_inspector(&self, view: ViewRef, open: bool) -> Result<(), WebViewError> {
        if !self.config.allow_inspector {
            return Err(WebViewError::Unsupported("inspector disabled".into()));
        }
        self.sync_view(&view, move |v| v.set_inspector(open)).await
    }

    async fn download_control(&self, view: ViewRef, _op: DownloadOp) -> Result<(), WebViewError> {
        self.views.get(&view)?;
        Err(WebViewError::Unsupported(
            "download control on desktop".into(),
        ))
    }

    fn control_events(&self, view: ViewRef) -> flume::Receiver<Result<ControlEvent, WebViewError>> {
        match self
            .views
            .get(&view)
            .and_then(|core| core.control.subscribe())
        {
            Ok(rx) => rx,
            Err(err) => failed_stream(err),
        }
    }

    fn telemetry(
        &self,
        view: ViewRef,
        kinds: Vec<TelemetryKind>,
    ) -> flume::Receiver<Result<TelemetryEvent, WebViewError>> {
        match self.views.get(&view) {
            Ok(core) => {
                let allowed: Vec<_> = kinds
                    .into_iter()
                    .filter(|k| core.spec.telemetry.contains(k))
                    .collect();
                core.telemetry.subscribe(allowed)
            }
            Err(err) => failed_stream(err),
        }
    }
}
