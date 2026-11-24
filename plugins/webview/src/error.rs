//! Domain error of the [`WebView`](crate::WebView) contract.

use istmo_core::IstmoError;
use istmo_macros::message;

/// Every failure a [`WebView`](crate::WebView) method reports.
#[message(crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WebViewError {
    /// The view was closed (or never existed).
    ViewClosed,
    /// The call presented a token that does not belong to the view.
    InvalidToken,
    /// The backend cannot do this; the message names what is missing.
    Unsupported(String),
    /// The requested [`RenderTarget`](crate::RenderTarget) is not
    /// available here (embedding on Wayland, a window on a platform
    /// without windows).
    RenderModeUnsupported,
    /// The requested [`WebProfile`](crate::WebProfile) cannot be
    /// isolated on this platform / OS version.
    ProfileUnsupported,
    /// The engine is not installed. Carries a URL the user can install
    /// it from (WebView2 bootstrapper), or the missing library name.
    RuntimeMissing(String),
    /// Another component of the process owns the toolkit the backend
    /// needs (an already initialised GTK on another thread).
    ToolkitBusy,
    /// A URL that does not parse or uses a blocked scheme.
    InvalidUrl(String),
    /// A malformed argument; the message names it.
    InvalidArgument(String),
    /// The main document's origin differs from the call's
    /// `expected_origin`; nothing was delivered.
    OriginMismatch,
    /// The request id is unknown, already answered or timed out.
    UnknownRequest,
    /// The engine did not answer in time.
    Timeout,
    /// Platform error; the message is the raw platform string.
    Backend(String),
}

impl std::fmt::Display for WebViewError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ViewClosed => f.write_str("web view is closed"),
            Self::InvalidToken => f.write_str("web view token does not match"),
            Self::Unsupported(what) => write!(f, "not supported by this web view backend: {what}"),
            Self::RenderModeUnsupported => {
                f.write_str("render target not supported by this web view backend")
            }
            Self::ProfileUnsupported => f.write_str("profile kind not supported on this platform"),
            Self::RuntimeMissing(hint) => write!(f, "web engine not installed: {hint}"),
            Self::ToolkitBusy => f.write_str("the UI toolkit is owned by another component"),
            Self::InvalidUrl(url) => write!(f, "invalid or blocked URL: {url}"),
            Self::InvalidArgument(what) => write!(f, "invalid argument: {what}"),
            Self::OriginMismatch => f.write_str("document origin does not match expected origin"),
            Self::UnknownRequest => f.write_str("unknown or already answered request"),
            Self::Timeout => f.write_str("web view did not answer in time"),
            Self::Backend(msg) => write!(f, "web view backend error: {msg}"),
        }
    }
}

impl std::error::Error for WebViewError {}

impl WebViewError {
    /// Recover the typed domain error from a failed client call.
    ///
    /// # Errors
    /// Hands back `err` unchanged when it is a transport failure or
    /// carries bytes that do not decode as a [`WebViewError`].
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

/// Lossy: transport failures flatten into [`WebViewError::Backend`]. Use
/// [`WebViewError::try_from_istmo`] to tell them apart.
impl From<IstmoError> for WebViewError {
    fn from(err: IstmoError) -> Self {
        Self::try_from_istmo(err).unwrap_or_else(|err| Self::Backend(err.to_string()))
    }
}
