//! Web view plugin for [`istmo`](https://docs.rs/istmo).
//!
//! One typed surface over Android `WebView`, `WKWebView` (iOS and
//! macOS), `WebView2` (Windows) and `WebKitGTK` / Qt `WebEngine` (Linux):
//! enough to build a browser, and the right knobs to host sign-in pages,
//! app content served from custom schemes, or a page talking to Rust
//! through a JS bridge.
//!
//! # Shape
//!
//! - [`WebViewClientExt::open_view`] creates a view and returns a
//!   [`ViewHandle`]. Dropping the handle closes the view.
//! - Views are [`RenderTarget::Embedded`] (a native child the app moves
//!   every frame through [`ViewHandle::presenter`]) or
//!   [`RenderTarget::Window`] (a window the plugin owns).
//! - Events come in two lanes: [`ViewHandle::control`] is lossless
//!   (navigation, decisions, scheme requests, bridge messages) and
//!   buffers until the first subscriber; [`ViewHandle::telemetry`] is
//!   lossy (progress, title, console) and only produced for the kinds a
//!   view asked for in [`WebViewSpec::telemetry`].
//! - Navigation rules ([`NavigationRules`]) run on the native side with
//!   the semantics of [`rules`]; [`RuleAction::Intercept`] captures
//!   redirects such as an OAuth callback.
//! - Every call carries the view's capability token, and the host
//!   refuses calls from other processes, so only the code that opened a
//!   view can drive it.
//!
//! # Sign-in
//!
//! For OAuth / OIDC prefer the system browser (`istmo-web-auth`):
//! several providers block embedded views, and RFC 8252 asks native apps
//! not to use them. An embedded view with an ephemeral profile and an
//! `Intercept` rule on the redirect URI remains available for providers
//! that allow it.

#![doc(html_root_url = "https://docs.rs/istmo-webview")]

#[cfg(any(
    all(
        any(target_os = "linux", target_os = "macos", target_os = "windows"),
        feature = "desktop"
    ),
    all(
        target_os = "linux",
        any(
            feature = "linux-webkit2gtk-4-1",
            feature = "linux-webkitgtk-6-0",
            feature = "linux-qt"
        )
    ),
    test
))]
pub mod desktop;
mod error;
mod handle;
pub mod host;
pub mod present;
pub mod rules;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
mod types;
mod unavailable;

pub use error::WebViewError;
pub use handle::{
    BridgeRequest, PendingDecision, PendingSchemeRequest, SchemeResponseWriter, ViewEvent,
    ViewEvents, ViewHandle, WebViewClientExt, WebViewSpecExt,
};
pub use present::Presenter;
pub use types::*;
pub use unavailable::{UnavailableWebView, UnavailableWebViewFactory};

use istmo_core::NativeHandleId;
use istmo_macros::{owned, plugin, stream};

/// Wire identifier of the web view plugin.
pub const WEBVIEW_PLUGIN_ID: &str = "istmo.webview";

/// The web view contract. Backends implement it natively (Kotlin,
/// Swift) or in Rust (desktop); apps use the generated
/// [`WebViewClient`] through [`WebViewClientExt`].
///
/// Every method taking a [`ViewRef`] fails with
/// [`WebViewError::InvalidToken`] when the token does not match and
/// [`WebViewError::ViewClosed`] for a closed view.
#[plugin(
    name = "istmo.webview",
    init = WebViewInitConfig,
    crate = "::istmo_core",
    remote = false,
)]
pub trait WebView {
    /// What this backend supports.
    async fn capabilities(&self) -> Result<WebViewCapabilities, WebViewError>;

    /// Start the engine ahead of the first view (process spawn, runtime
    /// load) so `open` is fast. Idempotent.
    async fn prewarm(&self) -> Result<(), WebViewError>;

    /// Create a view. It shows nothing until loaded and, when embedded,
    /// until its first frame.
    #[owned]
    async fn open(&self, spec: WebViewSpec) -> Result<WebViewInstance, WebViewError>;

    /// Close a view. Dropping its handle does the same.
    async fn close(&self, view: ViewRef) -> Result<(), WebViewError>;

    /// Placement of an embedded view. Sent as a notification by
    /// [`Presenter`]; hosts apply the latest one on their next frame.
    async fn set_frame(&self, view: ViewRef, frame: ViewFrame) -> Result<(), WebViewError>;

    /// Give the view keyboard focus.
    async fn focus(&self, view: ViewRef) -> Result<(), WebViewError>;

    /// Navigate the main frame. Rules apply to this load too.
    async fn load(&self, view: ViewRef, request: LoadRequest) -> Result<(), WebViewError>;

    /// Show `html` with `base_url` as its URL (and origin). With a bridge
    /// enabled, that origin must be listed in its `allowed_origins`.
    async fn load_html(
        &self,
        view: ViewRef,
        html: String,
        base_url: Option<String>,
    ) -> Result<(), WebViewError>;

    /// Back, forward, reload, stop.
    async fn navigate_history(&self, view: ViewRef, nav: HistoryNav) -> Result<(), WebViewError>;

    /// The back/forward list.
    async fn history(&self, view: ViewRef) -> Result<WebHistory, WebViewError>;

    /// Evaluate a script; resolves with the JSON of its result (a
    /// promise is awaited).
    async fn eval(&self, view: ViewRef, call: ScriptCall) -> Result<String, WebViewError>;

    /// Send a bridge message to the page.
    async fn post_message(
        &self,
        view: ViewRef,
        message: OutgoingMessage,
    ) -> Result<(), WebViewError>;

    /// Settle an `istmo.invoke` promise.
    async fn reply(&self, view: ViewRef, reply: BridgeReply) -> Result<(), WebViewError>;

    /// Answer a [`ControlEvent::DecisionRequested`].
    async fn decide(&self, view: ViewRef, decision: Decision) -> Result<(), WebViewError>;

    /// Start the response to a [`ControlEvent::SchemeRequest`].
    async fn scheme_head(
        &self,
        view: ViewRef,
        head: SchemeResponseHead,
    ) -> Result<(), WebViewError>;

    /// Continue or end a scheme response body.
    async fn scheme_chunk(&self, view: ViewRef, chunk: SchemeChunk) -> Result<(), WebViewError>;

    /// Body of a scheme request with
    /// [`SchemeRequestInfo::has_body`], in chunks.
    #[stream]
    fn scheme_request_body(&self, view: ViewRef, request_id: u64) -> Result<Vec<u8>, WebViewError>;

    /// Cookies of the view's profile.
    async fn cookies(
        &self,
        view: ViewRef,
        filter: CookieFilter,
    ) -> Result<Vec<WebCookie>, WebViewError>;

    /// Store a cookie in the view's profile.
    async fn set_cookie(&self, view: ViewRef, cookie: WebCookie) -> Result<(), WebViewError>;

    /// Remove stored data of the view's profile.
    async fn clear_data(
        &self,
        view: ViewRef,
        request: ClearDataRequest,
    ) -> Result<(), WebViewError>;

    /// Page zoom (`1.0` = 100 %).
    async fn set_zoom(&self, view: ViewRef, factor: f32) -> Result<(), WebViewError>;

    /// Find in page.
    async fn find(&self, view: ViewRef, query: FindQuery) -> Result<FindResult, WebViewError>;

    /// Encode the visible viewport.
    async fn snapshot(
        &self,
        view: ViewRef,
        options: SnapshotOptions,
    ) -> Result<WebSnapshot, WebViewError>;

    /// Open the platform print flow.
    async fn print(&self, view: ViewRef) -> Result<(), WebViewError>;

    /// Show or hide the inspector. Needs
    /// [`WebViewInitConfig::allow_inspector`].
    async fn set_inspector(&self, view: ViewRef, open: bool) -> Result<(), WebViewError>;

    /// Cancel, pause or resume a download.
    async fn download_control(&self, view: ViewRef, op: DownloadOp) -> Result<(), WebViewError>;

    /// The lossless event lane. One live subscriber per view.
    #[stream]
    fn control_events(&self, view: ViewRef) -> Result<ControlEvent, WebViewError>;

    /// The lossy event lane, filtered to `kinds` (intersected with
    /// [`WebViewSpec::telemetry`]).
    #[stream]
    fn telemetry(
        &self,
        view: ViewRef,
        kinds: Vec<TelemetryKind>,
    ) -> Result<TelemetryEvent, WebViewError>;
}

/// Handle id of `instance` with its token, for calls.
#[must_use]
pub const fn view_ref(instance: &WebViewInstance) -> ViewRef {
    ViewRef {
        view: instance.view,
        token_hi: instance.token_hi,
        token_lo: instance.token_lo,
    }
}

impl ViewRef {
    /// Handle id of the view.
    #[must_use]
    pub const fn id(&self) -> NativeHandleId {
        self.view
    }
}
