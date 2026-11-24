//! Host for platforms without a web engine backend.

use crate::{
    BridgeReply, ClearDataRequest, ControlEvent, CookieFilter, Decision, DownloadOp, FindQuery,
    FindResult, HistoryNav, LoadRequest, OutgoingMessage, SchemeChunk, SchemeResponseHead,
    ScriptCall, SnapshotOptions, TelemetryEvent, TelemetryKind, ViewFrame, ViewRef, WebCookie,
    WebHistory, WebSnapshot, WebView, WebViewCapabilities, WebViewError, WebViewFactory,
    WebViewInitConfig, WebViewInstance, WebViewSpec,
};

/// Answers every call with [`WebViewError::Unsupported`] (and reports
/// no capabilities), so shared app code can register the plugin on
/// every target and branch on [`WebView::capabilities`].
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableWebView;

/// Factory for [`UnavailableWebView`]:
/// `runtime.register_host(WebViewHost::new(UnavailableWebViewFactory))`.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableWebViewFactory;

impl WebViewFactory for UnavailableWebViewFactory {
    type Instance = UnavailableWebView;

    fn create(&self, _config: WebViewInitConfig) -> UnavailableWebView {
        UnavailableWebView
    }
}

fn unsupported<T>() -> Result<T, WebViewError> {
    Err(WebViewError::Unsupported(
        "no web engine on this platform".into(),
    ))
}

fn failed_stream<T>() -> flume::Receiver<Result<T, WebViewError>> {
    let (tx, rx) = flume::bounded(1);
    drop(tx.send(unsupported()));
    rx
}

impl WebView for UnavailableWebView {
    async fn capabilities(&self) -> Result<WebViewCapabilities, WebViewError> {
        Ok(WebViewCapabilities {
            backend: "unavailable".into(),
            ..WebViewCapabilities::default()
        })
    }
    async fn prewarm(&self) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn open(&self, _spec: WebViewSpec) -> Result<WebViewInstance, WebViewError> {
        unsupported()
    }
    async fn close(&self, _view: ViewRef) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn set_frame(&self, _view: ViewRef, _frame: ViewFrame) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn focus(&self, _view: ViewRef) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn load(&self, _view: ViewRef, _request: LoadRequest) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn load_html(
        &self,
        _view: ViewRef,
        _html: String,
        _base_url: Option<String>,
    ) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn navigate_history(&self, _view: ViewRef, _nav: HistoryNav) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn history(&self, _view: ViewRef) -> Result<WebHistory, WebViewError> {
        unsupported()
    }
    async fn eval(&self, _view: ViewRef, _call: ScriptCall) -> Result<String, WebViewError> {
        unsupported()
    }
    async fn post_message(
        &self,
        _view: ViewRef,
        _message: OutgoingMessage,
    ) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn reply(&self, _view: ViewRef, _reply: BridgeReply) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn decide(&self, _view: ViewRef, _decision: Decision) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn scheme_head(
        &self,
        _view: ViewRef,
        _head: SchemeResponseHead,
    ) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn scheme_chunk(&self, _view: ViewRef, _chunk: SchemeChunk) -> Result<(), WebViewError> {
        unsupported()
    }
    fn scheme_request_body(
        &self,
        _view: ViewRef,
        _request_id: u64,
    ) -> flume::Receiver<Result<Vec<u8>, WebViewError>> {
        failed_stream()
    }
    async fn cookies(
        &self,
        _view: ViewRef,
        _filter: CookieFilter,
    ) -> Result<Vec<WebCookie>, WebViewError> {
        unsupported()
    }
    async fn set_cookie(&self, _view: ViewRef, _cookie: WebCookie) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn clear_data(
        &self,
        _view: ViewRef,
        _request: ClearDataRequest,
    ) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn set_zoom(&self, _view: ViewRef, _factor: f32) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn find(&self, _view: ViewRef, _query: FindQuery) -> Result<FindResult, WebViewError> {
        unsupported()
    }
    async fn snapshot(
        &self,
        _view: ViewRef,
        _options: SnapshotOptions,
    ) -> Result<WebSnapshot, WebViewError> {
        unsupported()
    }
    async fn print(&self, _view: ViewRef) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn set_inspector(&self, _view: ViewRef, _open: bool) -> Result<(), WebViewError> {
        unsupported()
    }
    async fn download_control(&self, _view: ViewRef, _op: DownloadOp) -> Result<(), WebViewError> {
        unsupported()
    }
    fn control_events(
        &self,
        _view: ViewRef,
    ) -> flume::Receiver<Result<ControlEvent, WebViewError>> {
        failed_stream()
    }
    fn telemetry(
        &self,
        _view: ViewRef,
        _kinds: Vec<TelemetryKind>,
    ) -> flume::Receiver<Result<TelemetryEvent, WebViewError>> {
        failed_stream()
    }
}
