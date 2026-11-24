//! Browser-based sign-in for [`istmo`](https://docs.rs/istmo).
//!
//! OAuth 2.0 / `OpenID` Connect for native apps (RFC 8252): the
//! authorization page opens in the system browser — never in a view the
//! app controls — and the provider's redirect comes back to the app:
//!
//! | Platform | Browser | Callback |
//! |---|---|---|
//! | iOS / macOS (native shell) | `ASWebAuthenticationSession` | custom scheme or `https` (associated domain) |
//! | Android | Auth Tab, else Custom Tabs | custom scheme or verified `https` App Link |
//! | Desktop (`desktop` feature) | default browser | loopback `http://127.0.0.1:<port>` or app scheme |
//!
//! [`AuthMode::Embedded`] opens the page in an `istmo-webview` window
//! instead (desktop, `embedded` feature) for providers that allow it.
//! Google refuses embedded views; Microsoft allows them but loses single
//! sign-on.
//!
//! The plugin does not speak OAuth: it carries the authorization URL out
//! and the callback URL back, checking `state` (and `iss`) on the way.
//! [`pkce`] mints the verifier, `state` and `nonce`; the token exchange
//! stays with the app.
//!
//! ```ignore
//! use istmo_web_auth::{CallbackSpec, WebAuthClient, WebAuthClientExt, SignInOptions};
//!
//! let auth = WebAuthClient::acquire()?;
//! let flow = auth.begin(CallbackSpec::loopback("/callback")).await?;
//! let url = format!(
//!     "https://login.microsoftonline.com/common/oauth2/v2.0/authorize?client_id={id}\
//!      &response_type=code&redirect_uri={}&scope=openid&state={}&code_challenge={}\
//!      &code_challenge_method=S256",
//!     flow.redirect_uri_encoded(), flow.state(), flow.pkce().challenge(),
//! );
//! let callback = flow.complete(url, SignInOptions::default()).await?;
//! let code = callback.code()?; // exchange with `flow`'s verifier
//! ```

#![doc(html_root_url = "https://docs.rs/istmo-web-auth")]

mod callback;
mod client;
#[cfg(all(
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    feature = "desktop"
))]
pub mod desktop;
pub mod pkce;

pub use callback::{ProviderError, VerifiedCallback, parse_params};
pub use client::{SignInFlow, SignInOptions, WebAuthClientExt};

use istmo_core::{CancelToken, IstmoError};
use istmo_macros::{message, plugin};

/// Wire identifier of the web auth plugin.
pub const WEB_AUTH_PLUGIN_ID: &str = "istmo.web_auth";

/// What the active backend supports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent capability flags, one per platform query"
)]
pub struct WebAuthCapabilities {
    /// `aswebauthenticationsession`, `auth-tab`, `custom-tabs`,
    /// `system-browser`, …
    pub backend: String,
    pub loopback_callbacks: bool,
    pub scheme_callbacks: bool,
    /// `https` callbacks (associated domain / verified App Link).
    pub https_callbacks: bool,
    /// The browser session can skip shared cookies.
    pub ephemeral: bool,
    pub embedded: bool,
}

/// Where the provider redirects back to.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallbackSpec {
    /// `http://127.0.0.1:<random port><path>` (desktop only).
    Loopback(LoopbackCallback),
    /// `scheme://host/path`; the scheme must be declared in the app's
    /// `[[app.url_scheme]]`.
    CustomScheme(SchemeCallback),
    /// `https://host/path`, claimed through an associated domain (Apple)
    /// or a verified App Link (Android).
    Https(HttpsCallback),
}

impl CallbackSpec {
    /// Loopback callback on `path`.
    #[must_use]
    pub fn loopback(path: impl Into<String>) -> Self {
        Self::Loopback(LoopbackCallback { path: path.into() })
    }

    /// App-scheme callback `scheme://host/path`.
    #[must_use]
    pub fn scheme(
        scheme: impl Into<String>,
        host: impl Into<String>,
        path: impl Into<String>,
    ) -> Self {
        Self::CustomScheme(SchemeCallback {
            scheme: scheme.into(),
            host: host.into(),
            path: path.into(),
        })
    }
}

/// Loopback callback.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopbackCallback {
    /// Exact path the listener answers (`/callback`).
    pub path: String,
}

/// App-scheme callback.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeCallback {
    pub scheme: String,
    /// May be empty (`com.example.app:/callback`).
    pub host: String,
    pub path: String,
}

/// `https` callback.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpsCallback {
    pub host: String,
    pub path: String,
}

/// Answer of [`WebAuth::prepare`].
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct PreparedRedirect {
    /// The exact `redirect_uri` to put in the authorization request.
    pub redirect_uri: String,
    /// Single-use id of the prepared callback.
    #[redact]
    pub session: u64,
}

/// How the authorization page is shown.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AuthMode {
    /// The system browser (recommended).
    #[default]
    System,
    /// A web view the plugin controls. Several providers refuse it.
    Embedded,
}

/// One sign-in.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct AuthRequest {
    /// From [`PreparedRedirect::session`].
    #[redact]
    pub session: u64,
    /// The provider's authorization URL, `https` only.
    #[redact]
    pub authorize_url: String,
    /// The `state` sent; callbacks carrying another one are ignored.
    #[redact]
    pub expected_state: String,
    pub mode: AuthMode,
    /// Do not share cookies with the user's browser session.
    pub ephemeral: bool,
    /// Give up after this long; `0` = no limit (the user may still
    /// cancel).
    pub timeout_ms: u32,
}

/// The provider's redirect.
#[message(crate = "::istmo_core")]
#[derive(Clone, PartialEq, Eq)]
pub struct AuthCallback {
    /// Full callback URL, query (and fragment) included.
    #[redact]
    pub url: String,
}

/// Failure of a sign-in. A provider's own refusal (`error=access_denied`)
/// is not one: it arrives inside the [`AuthCallback`].
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WebAuthError {
    /// The user closed the browser or the app cancelled.
    Cancelled,
    /// Another sign-in is running; platforms allow one at a time.
    AlreadyInProgress,
    /// No browser can show the page.
    NoBrowser,
    /// Nothing to anchor the browser sheet to (no window / activity).
    PresentationUnavailable,
    /// The callback scheme or domain is not registered by the app.
    CallbackNotRegistered,
    /// The callback's `state` differs from the request's.
    StateMismatch,
    /// The callback's `iss` differs from the expected issuer.
    IssuerMismatch,
    /// The callback carries no authorization code.
    MissingCode,
    Timeout,
    /// The prepared redirect is unknown or was already used.
    UnknownSession,
    Unsupported(String),
    InvalidArgument(String),
    Backend(String),
}

impl std::fmt::Display for WebAuthError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Cancelled => f.write_str("sign-in cancelled"),
            Self::AlreadyInProgress => f.write_str("another sign-in is in progress"),
            Self::NoBrowser => f.write_str("no browser can show the sign-in page"),
            Self::PresentationUnavailable => f.write_str("no window to present the sign-in on"),
            Self::CallbackNotRegistered => {
                f.write_str("the callback scheme or domain is not registered by the app")
            }
            Self::StateMismatch => f.write_str("callback state does not match the request"),
            Self::IssuerMismatch => f.write_str("callback issuer does not match"),
            Self::MissingCode => f.write_str("callback carries no authorization code"),
            Self::Timeout => f.write_str("sign-in timed out"),
            Self::UnknownSession => f.write_str("unknown or already used redirect"),
            Self::Unsupported(what) => write!(f, "not supported: {what}"),
            Self::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            Self::Backend(msg) => write!(f, "web auth backend error: {msg}"),
        }
    }
}

impl std::error::Error for WebAuthError {}

impl WebAuthError {
    /// Recover the typed domain error from a failed client call.
    ///
    /// # Errors
    /// Hands back `err` unchanged when it is a transport failure or
    /// carries bytes that do not decode as a [`WebAuthError`].
    pub fn try_from_istmo(err: IstmoError) -> Result<Self, IstmoError> {
        match err {
            IstmoError::PluginError { bytes } => match istmo_core::codec::decode::<Self>(&bytes) {
                Ok((domain, _)) => Ok(domain),
                Err(_) => Err(IstmoError::PluginError { bytes }),
            },
            other => Err(other),
        }
    }
}

/// Lossy: transport failures flatten into [`WebAuthError::Backend`].
impl From<IstmoError> for WebAuthError {
    fn from(err: IstmoError) -> Self {
        Self::try_from_istmo(err).unwrap_or_else(|err| Self::Backend(err.to_string()))
    }
}

/// The web auth contract.
#[plugin(name = "istmo.web_auth", crate = "::istmo_core", remote = false)]
pub trait WebAuth {
    /// What this backend supports.
    async fn capabilities(&self) -> Result<WebAuthCapabilities, WebAuthError>;

    /// Reserve a callback and return the `redirect_uri` to send. A
    /// loopback callback starts listening here, so the port is known
    /// before the authorization URL is built.
    async fn prepare(&self, callback: CallbackSpec) -> Result<PreparedRedirect, WebAuthError>;

    /// Show the authorization page and wait for the callback whose
    /// `state` matches. One sign-in at a time.
    async fn authenticate(
        &self,
        request: AuthRequest,
        cancel: CancelToken,
    ) -> Result<AuthCallback, WebAuthError>;
}
