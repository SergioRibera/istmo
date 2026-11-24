//! Wire types of the [`WebView`](crate::WebView) contract.
//!
//! Everything here crosses the istmo bus and is generated into Kotlin
//! and Swift, so the shapes stay within what the contract extractor
//! models: named-field structs, unit or tuple enum variants, `Vec`,
//! `Option`, fixed-width integers. Names carry a `Web` prefix where a
//! bare name would collide in Swift's flat app module.

#![expect(
    missing_docs,
    reason = "wire fields and variants are named after what they carry; the non-obvious ones are documented"
)]

use istmo_core::NativeHandleId;
use istmo_macros::message;

/// Phantom tag for the native view handle.
#[derive(Debug, Clone, Copy)]
pub struct WebViewTag(std::convert::Infallible);

// -------------------------------------------------------------------
// Plugin-wide configuration
// -------------------------------------------------------------------

/// Plugin-wide settings, fixed for the lifetime of a client instance.
///
/// Custom schemes live here rather than per view: `WebView2` registers
/// them when its environment is created, and every view of the
/// environment shares the registration.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebViewInitConfig {
    /// Schemes the app serves to its views.
    pub schemes: Vec<CustomScheme>,
    /// Allow [`WebView::set_inspector`](crate::WebView::set_inspector)
    /// and console telemetry. Release builds should leave it off; on
    /// Android it enables debugging for every web view of the process.
    pub allow_inspector: bool,
    /// Appended to the engine's default user agent (`MyApp/1.2`).
    pub user_agent_suffix: Option<String>,
}

/// One app-served scheme.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CustomScheme {
    /// Lowercase scheme name without `:` (`app`).
    pub name: String,
    pub source: SchemeSource,
}

/// Where a custom scheme's responses come from.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemeSource {
    /// Files under a directory of the app's bundled istmo assets,
    /// served by the native side without a round trip to Rust.
    Assets(AssetsSource),
    /// Every request becomes a [`ControlEvent::SchemeRequest`] the app
    /// answers with [`WebView::scheme_head`](crate::WebView::scheme_head)
    /// and [`WebView::scheme_chunk`](crate::WebView::scheme_chunk).
    Dynamic,
}

/// Directory of bundled assets backing a scheme.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AssetsSource {
    /// Path relative to the app's asset root, `/`-separated.
    pub dir: String,
}

// -------------------------------------------------------------------
// Capabilities
// -------------------------------------------------------------------

/// What the active backend supports. Every flag is a runtime fact: the
/// same build reports different values on different OS versions.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capability flags, one per platform query"
)]
pub struct WebViewCapabilities {
    /// Engine in use (`android-webview`, `wkwebview`, `webview2`,
    /// `webkit2gtk-4.1`, …).
    pub backend: String,
    /// Engine version, empty when unknown.
    pub engine_version: String,
    pub embedded: bool,
    pub windowed: bool,
    /// A [`WindowTarget::parent_window_id`] is honoured.
    pub window_parenting: bool,
    pub persistent_profiles: bool,
    /// Ephemeral profiles are isolated from every other profile.
    pub ephemeral_profiles: bool,
    pub isolated_world: bool,
    /// The JS bridge answers `istmo.invoke` with a reply.
    pub bridge_reply: bool,
    /// A navigation can wait for [`RuleAction::Ask`] without being
    /// cancelled and re-issued (POST bodies survive).
    pub deferred_navigation: bool,
    /// Scheme handlers see request bodies.
    pub scheme_request_bodies: bool,
    pub downloads: bool,
    /// Downloads can be paused and resumed.
    pub download_control: bool,
    pub find: bool,
    pub print: bool,
    pub snapshot: bool,
    pub inspector: bool,
    /// Origin pages loaded from each custom scheme get. Differs per
    /// platform (Android serves schemes from an `https` host), so
    /// bridge `allowed_origins` should be built from it.
    pub scheme_origins: Vec<SchemeOrigin>,
}

/// Effective origin of a custom scheme on this platform.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeOrigin {
    pub scheme: String,
    /// `app://localhost`, `https://appassets.androidplatform.net`, …
    pub origin: String,
    /// URL prefix to which paths are appended.
    pub base_url: String,
}

// -------------------------------------------------------------------
// View creation
// -------------------------------------------------------------------

/// Everything a view is created with.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebViewSpec {
    pub target: RenderTarget,
    pub profile: WebProfile,
    pub settings: WebViewSettings,
    pub rules: NavigationRules,
    pub user_scripts: Vec<UserScript>,
    pub bridge: Option<BridgeConfig>,
    pub timeouts: DecisionTimeouts,
    /// Telemetry kinds the native side produces at all. Kinds nobody
    /// asked for are never encoded.
    pub telemetry: Vec<TelemetryKind>,
}

/// Where a view is shown.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenderTarget {
    /// Child of an app window registered in `istmo-window` (desktop) or
    /// laid over the app's activity / root view (mobile). The app moves
    /// it every frame through the view's presenter.
    Embedded(EmbeddedTarget),
    /// A top-level window the plugin owns (a sign-in popup, a separate
    /// browser window). Full screen on mobile.
    Window(WindowTarget),
}

/// Target of an embedded view.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EmbeddedTarget {
    /// `istmo-window` id of the parent window. Ignored on mobile.
    pub window_id: u64,
}

/// Target of a plugin-owned window.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowTarget {
    pub title: String,
    /// Logical pixels.
    pub width: u32,
    pub height: u32,
    /// Keep the window above this registered window (transient /
    /// owned window). Best effort, see
    /// [`WebViewCapabilities::window_parenting`].
    pub parent_window_id: Option<u64>,
    pub modal: bool,
    pub resizable: bool,
}

/// Storage a view uses for cookies, cache and web storage.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebProfile {
    /// Nothing is written to disk and nothing is shared with other
    /// profiles. Platforms that cannot isolate it fail with
    /// [`WebViewError::ProfileUnsupported`](crate::WebViewError::ProfileUnsupported).
    Ephemeral,
    /// Named profile persisted under the app's data directory.
    Persistent(PersistentProfile),
    /// The platform's default profile, shared by every view that asks
    /// for it.
    Shared,
}

/// A named persistent profile.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistentProfile {
    /// 1–64 characters of `[A-Za-z0-9._-]`.
    pub name: String,
}

/// Per-view engine settings. [`Default`] is the locked-down baseline.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent engine switches mirroring each platform's settings object"
)]
pub struct WebViewSettings {
    pub javascript: bool,
    pub dom_storage: bool,
    /// Media may start without a user gesture.
    pub media_autoplay: bool,
    /// Pinch / ctrl+wheel zoom.
    pub zoom_gestures: bool,
    pub context_menu: ContextMenuMode,
    /// Replaces the user agent. Never spoof another browser to get past
    /// a provider's embedded-view block: it breaks their policies.
    pub user_agent: Option<String>,
    pub accept_language: Option<String>,
    pub third_party_cookies: bool,
    /// `file:` URLs may load. Off by default.
    pub file_access: bool,
    /// Password and form autofill.
    pub autofill: bool,
    /// `WebAuthn` / passkeys where the platform allows them in a view.
    pub webauthn: bool,
    /// Draw nothing behind transparent page content.
    pub transparent: bool,
    pub background: WebColor,
}

impl Default for WebViewSettings {
    fn default() -> Self {
        Self {
            javascript: true,
            dom_storage: true,
            media_autoplay: false,
            zoom_gestures: true,
            context_menu: ContextMenuMode::Native,
            user_agent: None,
            accept_language: None,
            third_party_cookies: false,
            file_access: false,
            autofill: false,
            webauthn: true,
            transparent: false,
            background: WebColor::WHITE,
        }
    }
}

/// What a right click / long press shows.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ContextMenuMode {
    /// The engine's own menu.
    #[default]
    Native,
    /// Nothing.
    Hidden,
}

/// 8-bit sRGB colour with alpha.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebColor {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl WebColor {
    pub const WHITE: Self = Self {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    };
    pub const TRANSPARENT: Self = Self {
        r: 0,
        g: 0,
        b: 0,
        a: 0,
    };
}

/// Script injected into every matching document.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserScript {
    pub source: String,
    pub inject_at: InjectionTime,
    pub main_frame_only: bool,
    pub world: ScriptWorld,
    /// Origins (`https://example.com`) the script runs in. Empty means
    /// every origin.
    pub allowed_origins: Vec<String>,
}

/// When a user script runs.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionTime {
    DocumentStart,
    DocumentEnd,
}

/// JavaScript world a script runs in.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ScriptWorld {
    /// The page's own globals.
    #[default]
    Page,
    /// A world isolated from page scripts (where supported, see
    /// [`WebViewCapabilities::isolated_world`]).
    Isolated,
}

/// The page-side `istmo` object.
///
/// Pages get `istmo.postMessage(channel, data)` and
/// `await istmo.invoke(channel, data)` (`data`: string or `Uint8Array`)
/// only when their origin is listed. The origin of every message comes
/// from the engine, never from the page.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeConfig {
    pub channels: Vec<String>,
    /// Exact origins (`https://app.example.com`, a
    /// [`SchemeOrigin::origin`]). Opaque and `null` origins never match.
    pub allowed_origins: Vec<String>,
    /// Also expose the bridge to iframes of allowed origins.
    pub allow_subframes: bool,
}

/// How long the native side waits for the app before taking the safe
/// default (deny / cancel / `504`).
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecisionTimeouts {
    /// [`RuleAction::Ask`] navigations and new-window requests.
    pub navigation_ms: u32,
    /// First [`WebView::scheme_head`](crate::WebView::scheme_head)
    /// of a dynamic scheme request.
    pub scheme_head_ms: u32,
}

impl Default for DecisionTimeouts {
    fn default() -> Self {
        Self {
            navigation_ms: 1_000,
            scheme_head_ms: 10_000,
        }
    }
}

// -------------------------------------------------------------------
// Navigation rules
// -------------------------------------------------------------------

/// Evaluated natively, in order, for every top-level navigation and
/// new-window request; the first matching rule decides.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationRules {
    /// Action when no rule matches.
    pub default_action: RuleAction,
    pub rules: Vec<NavigationRule>,
}

impl Default for NavigationRules {
    /// Allow everything except the schemes the plugin always blocks.
    fn default() -> Self {
        Self {
            default_action: RuleAction::Allow,
            rules: Vec::new(),
        }
    }
}

/// One rule.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NavigationRule {
    pub pattern: UrlPattern,
    pub action: RuleAction,
}

/// What happens to a matching navigation.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuleAction {
    Allow,
    Deny,
    /// Cancel and report [`ControlEvent::Intercepted`] (redirect capture).
    Intercept,
    /// Cancel and open in the system browser.
    External,
    /// Ask the app through [`DecisionKind::Navigation`].
    Ask,
}

/// URL matcher. See [`crate::rules`] for the normalisation every
/// backend applies before comparing.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UrlPattern {
    /// Lowercase scheme, `*` for any.
    pub scheme: String,
    /// Exact host, `*.example.com` for strict subdomains, `*` for any.
    pub host: String,
    /// `None` = the scheme's default port only.
    pub port: Option<u16>,
    /// Matched on path segment boundaries; empty matches every path.
    pub path_prefix: String,
}

// -------------------------------------------------------------------
// View identity
// -------------------------------------------------------------------

/// Answer of [`WebView::open`](crate::WebView::open). Prefer the
/// ergonomic `open_view`, which wraps it in a [`ViewHandle`](crate::ViewHandle).
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct WebViewInstance {
    #[handle(WebViewTag)]
    pub view: NativeHandleId,
    /// Capability token every later call presents.
    #[redact]
    pub token_hi: u64,
    #[redact]
    pub token_lo: u64,
}

/// A view and its capability token. Calls with a wrong token fail with
/// [`WebViewError::InvalidToken`](crate::WebViewError::InvalidToken).
#[message(crate = "::istmo_core")]
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct ViewRef {
    pub view: NativeHandleId,
    pub token_hi: u64,
    pub token_lo: u64,
}

impl std::fmt::Debug for ViewRef {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ViewRef")
            .field("view", &self.view)
            .finish_non_exhaustive()
    }
}

/// Placement of an embedded view, in physical pixels of its parent.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ViewFrame {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub visible: bool,
}

// -------------------------------------------------------------------
// Navigation
// -------------------------------------------------------------------

/// A top-level load.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct LoadRequest {
    #[redact]
    pub url: String,
    /// Sent with this request only, never on cross-origin redirects.
    pub headers: Vec<HttpHeader>,
    /// `POST` body; `None` loads with `GET`.
    pub post_body: Option<Vec<u8>>,
}

impl LoadRequest {
    /// `GET url`.
    #[must_use]
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            headers: Vec::new(),
            post_body: None,
        }
    }
}

/// One HTTP header.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpHeader {
    pub name: String,
    pub value: String,
}

/// History movement.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryNav {
    Back,
    Forward,
    Reload,
    ReloadBypassCache,
    Stop,
    /// Relative jump in the back/forward list.
    Offset(i32),
}

/// The back/forward list.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct WebHistory {
    pub entries: Vec<HistoryEntry>,
    /// Index of the current entry in `entries`.
    pub current: u32,
}

/// One history entry.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    pub url: String,
    pub title: String,
}

// -------------------------------------------------------------------
// Scripts and the bridge
// -------------------------------------------------------------------

/// Script evaluation. The script only runs when the committed main
/// document has `expected_origin`, so a page that navigated away in the
/// meantime never sees it.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptCall {
    pub source: String,
    pub world: ScriptWorld,
    pub expected_origin: String,
}

/// App → page bridge message, delivered as a `message` event on
/// `istmo` when the committed main document has `expected_origin`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingMessage {
    pub channel: String,
    pub data: Vec<u8>,
    pub expected_origin: String,
}

/// Answer to an `istmo.invoke` call.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeReply {
    pub reply_id: u64,
    pub result: ReplyResult,
}

/// Resolves (`Ok`) or rejects (`Err`, the message of the JS `Error`)
/// the page's promise.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReplyResult {
    Ok(Vec<u8>),
    Err(String),
}

// -------------------------------------------------------------------
// Decisions
// -------------------------------------------------------------------

/// Answer to a [`DecisionRequest`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decision {
    pub request_id: u64,
    pub response: DecisionResponse,
}

/// The app's choice. A response that does not fit the request's kind is
/// treated as [`DecisionResponse::Deny`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionResponse {
    Allow,
    Deny,
    /// Grant only these permissions of a [`DecisionKind::Permission`].
    Grant(Vec<WebPermission>),
    /// Answer an HTTP auth challenge.
    Credentials(HttpCredentials),
    /// Files picked for a [`DecisionKind::FileChooser`].
    Files(Vec<String>),
    /// Accept a dialog; a prompt's text when present.
    DialogAccept(Option<String>),
    /// Save a download to this path.
    DownloadTo(String),
}

/// HTTP auth credentials.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct HttpCredentials {
    pub user: String,
    pub password: String,
}

impl std::fmt::Debug for HttpCredentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HttpCredentials").finish_non_exhaustive()
    }
}

// -------------------------------------------------------------------
// Custom scheme responses
// -------------------------------------------------------------------

/// Status line and headers of a dynamic scheme response. The body
/// follows as [`SchemeChunk`]s.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeResponseHead {
    pub request_id: u64,
    pub status: u16,
    pub mime_type: String,
    pub headers: Vec<HttpHeader>,
    pub content_length: Option<u64>,
}

/// One piece of a dynamic scheme response body.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeChunk {
    pub request_id: u64,
    pub body: SchemeBody,
}

/// Body piece.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SchemeBody {
    Data(Vec<u8>),
    End,
    /// Fail the request mid-body.
    Abort(String),
}

// -------------------------------------------------------------------
// Profile data
// -------------------------------------------------------------------

/// A cookie.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct WebCookie {
    pub name: String,
    pub value: String,
    pub domain: String,
    pub path: String,
    /// Expiry as Unix milliseconds; `None` = session cookie.
    pub expires_unix_ms: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: SameSite,
}

impl std::fmt::Debug for WebCookie {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WebCookie")
            .field("name", &self.name)
            .field("domain", &self.domain)
            .field("path", &self.path)
            .finish_non_exhaustive()
    }
}

/// `SameSite` attribute.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SameSite {
    #[default]
    Lax,
    Strict,
    None,
}

/// Which cookies [`WebView::cookies`](crate::WebView::cookies) returns.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CookieFilter {
    /// Cookies that would be sent to this URL; `None` = every cookie
    /// of the view's profile.
    pub url: Option<String>,
    pub name: Option<String>,
}

/// What [`WebView::clear_data`](crate::WebView::clear_data) removes.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearDataRequest {
    pub kinds: Vec<DataKind>,
    /// Only data modified after this Unix time (ms); `None` = all.
    pub since_unix_ms: Option<i64>,
}

/// Kind of stored data.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DataKind {
    Cookies,
    Cache,
    LocalStorage,
    IndexedDb,
    ServiceWorkers,
    All,
}

// -------------------------------------------------------------------
// Misc requests
// -------------------------------------------------------------------

/// Find-in-page query.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindQuery {
    /// Empty clears the highlight.
    pub text: String,
    pub backwards: bool,
    pub case_sensitive: bool,
    pub wrap: bool,
}

/// Find-in-page result.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FindResult {
    pub matches: u32,
    /// 1-based index of the highlighted match, `0` when none.
    pub active: u32,
}

/// Snapshot request.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Downscale so the width is at most this; `0` keeps the size.
    pub max_width: u32,
    pub format: SnapshotFormat,
}

/// Snapshot encoding.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotFormat {
    Png,
    /// JPEG with quality `0..=100`.
    Jpeg(u8),
}

/// Encoded snapshot of the visible viewport.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebSnapshot {
    pub width: u32,
    pub height: u32,
    pub format: SnapshotFormat,
    pub bytes: Vec<u8>,
}

/// Download control.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadOp {
    pub download_id: u64,
    pub action: DownloadAction,
}

/// What to do with a download.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DownloadAction {
    Cancel,
    Pause,
    Resume,
}

// -------------------------------------------------------------------
// Control events
// -------------------------------------------------------------------

/// Lossless events of one view: the native side buffers them until the
/// first subscriber and never drops one.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ControlEvent {
    NavigationStarted(NavInfo),
    /// The main document of the navigation is now displayed; its origin
    /// is the one bridge messages and `expected_origin` checks use.
    NavigationCommitted(NavInfo),
    NavigationFinished(NavInfo),
    NavigationFailed(NavFailure),
    /// A navigation matched a [`RuleAction::Intercept`] rule and was
    /// cancelled.
    Intercepted(InterceptedNav),
    DecisionRequested(DecisionRequest),
    SchemeRequest(SchemeRequestInfo),
    Message(BridgeMessage),
    /// The page opened a window and the app allowed it; the view was
    /// created with the opener's profile and rules. Adopt it with
    /// `ViewHandle::adopt`.
    NewView(NewViewInfo),
    /// `window.close()`.
    CloseRequested,
    FullscreenChanged(bool),
    /// The engine's content process died. Pending decisions were denied
    /// and the bridge reset; reload or close the view.
    ProcessTerminated(TerminationInfo),
}

/// A navigation.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct NavInfo {
    pub navigation_id: u64,
    #[redact]
    pub url: String,
    pub is_redirect: bool,
    pub user_gesture: bool,
}

/// A failed navigation.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct NavFailure {
    pub navigation_id: u64,
    #[redact]
    pub url: String,
    pub kind: NavErrorKind,
    /// Platform description, for logs.
    pub description: String,
}

/// Why a navigation failed.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavErrorKind {
    Network,
    Dns,
    Timeout,
    Tls,
    /// A rule, the app or the user cancelled it.
    Cancelled,
    /// The URL was malformed or its scheme is blocked.
    BadUrl,
    Other,
}

/// A navigation an [`RuleAction::Intercept`] rule captured.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct InterceptedNav {
    pub navigation_id: u64,
    #[redact]
    pub url: String,
}

/// Something the app has to decide. Answer with
/// [`WebView::decide`](crate::WebView::decide) using `request_id`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRequest {
    pub request_id: u64,
    pub kind: DecisionKind,
}

/// Kind of decision.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionKind {
    /// A navigation matched [`RuleAction::Ask`].
    Navigation(NavInfo),
    NewWindow(NewWindowAsk),
    Permission(PermissionAsk),
    HttpAuth(HttpAuthAsk),
    /// Certificate error. Deny unless the user explicitly accepts this
    /// certificate for this host.
    TlsError(TlsErrorInfo),
    FileChooser(FileChooserAsk),
    Dialog(DialogAsk),
    Download(DownloadAsk),
}

/// `window.open` / `target=_blank`.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct NewWindowAsk {
    #[redact]
    pub url: String,
    pub user_gesture: bool,
}

/// A page asks for device access.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionAsk {
    pub permissions: Vec<WebPermission>,
    pub frame_origin: String,
    pub top_origin: String,
}

/// Web permission.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WebPermission {
    Camera,
    Microphone,
    Geolocation,
    Notifications,
    Clipboard,
    Midi,
    ProtectedMedia,
    Other,
}

/// HTTP authentication challenge.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpAuthAsk {
    pub host: String,
    pub realm: String,
    pub is_proxy: bool,
}

/// Certificate error details.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct TlsErrorInfo {
    #[redact]
    pub url: String,
    pub host: String,
    pub description: String,
    /// Hex SHA-256 of the leaf certificate, empty when unavailable.
    pub fingerprint_sha256: String,
}

/// `<input type=file>`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChooserAsk {
    pub multiple: bool,
    /// MIME types / extensions from the `accept` attribute.
    pub accept: Vec<String>,
    pub capture: bool,
}

/// `alert` / `confirm` / `prompt` / `beforeunload`.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogAsk {
    pub kind: DialogKind,
    pub message: String,
    pub default_text: String,
    pub origin: String,
}

/// Dialog kind.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    Alert,
    Confirm,
    Prompt,
    BeforeUnload,
}

/// A download the page started.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct DownloadAsk {
    pub download_id: u64,
    #[redact]
    pub url: String,
    /// Sanitised file name (no path separators).
    pub suggested_name: String,
    pub mime_type: String,
    pub content_length: Option<u64>,
}

/// A request to a [`SchemeSource::Dynamic`] scheme.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct SchemeRequestInfo {
    pub request_id: u64,
    pub scheme: String,
    pub method: String,
    #[redact]
    pub url: String,
    pub headers: Vec<HttpHeader>,
    /// Fetch it with [`WebView::scheme_request_body`](crate::WebView::scheme_request_body).
    pub has_body: bool,
    /// Origin of the document that issued the request, when known.
    pub initiator_origin: Option<String>,
}

/// Page → app bridge message.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BridgeMessage {
    pub channel: String,
    /// Origin of the sending frame, as the engine reports it.
    pub origin: String,
    pub is_main_frame: bool,
    /// Document that sent it. Messages from replaced documents are
    /// dropped before they get here.
    pub document_id: u64,
    /// `Some` for `istmo.invoke`: answer with [`WebView::reply`](crate::WebView::reply).
    pub reply_id: Option<u64>,
    pub data: Vec<u8>,
}

/// A view the page opened.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct NewViewInfo {
    pub instance: WebViewInstance,
    #[redact]
    pub url: String,
}

/// Why the content process ended.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminationInfo {
    pub reason: TerminationReason,
}

/// Content process end reason.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminationReason {
    Crashed,
    /// The OS reclaimed it (memory pressure).
    Killed,
    Other,
}

// -------------------------------------------------------------------
// Telemetry
// -------------------------------------------------------------------

/// Telemetry kinds a subscriber asks for.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TelemetryKind {
    Progress,
    Title,
    Url,
    Favicon,
    History,
    Console,
    Downloads,
}

/// Lossy events.
///
/// Progress, title, URL, favicon and history keep only their latest
/// value; console lines and download progress go through a
/// ring that drops the oldest entry and reports how many with
/// [`TelemetryEvent::Dropped`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub enum TelemetryEvent {
    /// `0.0..=1.0`.
    Progress(f32),
    Title(String),
    /// Committed URL of the main document.
    Url(String),
    Favicon(String),
    History(HistoryState),
    Console(ConsoleLine),
    DownloadUpdate(DownloadProgress),
    DownloadFinished(DownloadDone),
    /// Events lost to the ring since the previous delivery.
    Dropped(u32),
}

/// Back/forward availability.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryState {
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

/// A console message.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConsoleLine {
    pub level: ConsoleLevel,
    pub message: String,
    pub source: String,
    pub line: u32,
}

/// Console level.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConsoleLevel {
    Debug,
    Log,
    Info,
    Warning,
    Error,
}

/// Download progress.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DownloadProgress {
    pub download_id: u64,
    pub received: u64,
    pub total: Option<u64>,
}

/// Download end.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadDone {
    pub download_id: u64,
    /// Saved file; `None` on failure or cancel.
    pub path: Option<String>,
    pub error: Option<String>,
}
