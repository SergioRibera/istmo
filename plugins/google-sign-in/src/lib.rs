//! Google Sign-In plugin for [`istmo`](https://docs.rs/istmo).
//!
//! Ships a Rust-side [`SignIn`] trait plus the wire types needed to
//! drive a native `GoogleSignInClient` on Android and `ASAuthorization`
//! flow on Apple platforms. Credentials returned by the platform side
//! are opaque [`NativeHandle`](istmo_core::NativeHandle)s — the native
//! layer owns the token material and releases it when the handle is
//! dropped.
//!
//! The `codegen` feature exposes the plugin's `Contract`
//! (see [`istmo-build`](https://docs.rs/istmo-build)) so downstream
//! apps that don't consume the build-script handover can still
//! generate matching Kotlin / Swift bindings.

#![doc(html_root_url = "https://docs.rs/istmo-google-sign-in")]

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_core::NativeHandleId;
use istmo_macros::{message, owned, plugin};

/// Wire identifier for the Google Sign-In plugin.
pub const GOOGLE_SIGN_IN_PLUGIN_ID: &str = "istmo.google_sign_in";

#[non_exhaustive]
#[derive(Debug)]
pub enum Credential {}

/// How the platform should behave when no cached credential is
/// available.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum SignInMode {
    /// Present the account chooser sheet (Credential Manager on
    /// Android, `ASAuthorizationController` on iOS).
    Interactive,
    /// Return a credential only if the platform already has one
    /// cached. Fail with `SignInError::NoCredentialAvailable` rather
    /// than prompting the user.
    SilentOnly,
}

/// Instance-scoped OAuth configuration for the sign-in plugin.
///
/// Passed as `init` when acquiring a `SignInClient` — the native
/// backend hangs on to the config for the lifetime of the instance.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignInConfig {
    /// Google Cloud Console **Web** OAuth client id. Used verbatim as
    /// `serverClientId` on Android and as the ID-token audience on
    /// iOS.
    pub server_client_id: String,
    /// Extra OAuth scopes to request on top of the default `openid` /
    /// `email` / `profile` set.
    pub scopes: Vec<String>,
    /// Restrict sign-in to accounts belonging to this Google Workspace
    /// domain. `None` allows any account.
    pub hosted_domain: Option<String>,
    /// Optional nonce for replay protection. Recommended when the
    /// resulting `id_token` is forwarded to your own backend.
    pub nonce: Option<String>,
    /// If `true`, sign the user in without a chooser when exactly one
    /// eligible credential is available. Ignored when
    /// `SignInMode::Interactive` is requested.
    pub auto_select: bool,
}

/// A successfully authenticated Google account.
///
/// `credential` is an opaque handle that owns the underlying platform
/// token material — obtain the ergonomic `OwnedSignInAccount` via
/// `sign_in_owned` / `silent_sign_in_owned` to have it dropped
/// deterministically.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignInAccount {
    /// Stable Google user id (`sub` claim of the id token).
    pub id: String,
    /// Verified email address, if the user granted the `email` scope.
    pub email: Option<String>,
    /// Full display name, if the user granted the `profile` scope.
    pub display_name: Option<String>,
    /// Profile photo URL, if available.
    pub photo_url: Option<String>,
    /// Signed OpenID Connect id token — forward to your backend to
    /// verify the user's identity server-side.
    pub id_token: String,
    /// Scopes actually granted by the user (may be a subset of what
    /// `SignInConfig::scopes` requested).
    pub granted_scopes: Vec<String>,
    /// Opaque credential handle. Native side releases the underlying
    /// token when `Frame::ReleaseNativeHandle` arrives — usually via
    /// `NativeHandle::drop` on the owned wrapper.
    #[handle(Credential)]
    pub credential: NativeHandleId,
}

/// Domain-level errors surfaced by every `SignIn` method.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SignInError {
    /// User dismissed the account chooser or denied consent.
    UserCancelled,
    /// [`SignInMode::SilentOnly`] was requested but the platform has
    /// no cached credential.
    NoCredentialAvailable,
    /// The credential has expired or was invalidated. Prompt the user
    /// for interactive sign-in.
    Reauthenticate,
    /// Config-level failure — bad client id, missing scope, etc.
    InvalidConfiguration(String),
    /// Transient network failure while contacting Google. Retry-safe.
    Network(String),
    /// Any other backend failure. Message is the raw platform error.
    Backend(String),
}

impl SignInConfig {
    pub fn builder(server_client_id: impl Into<String>) -> SignInConfigBuilder {
        SignInConfigBuilder(Self {
            server_client_id: server_client_id.into(),
            scopes: Vec::new(),
            hosted_domain: None,
            nonce: None,
            auto_select: false,
        })
    }
}

#[derive(Debug, Clone)]
#[must_use]
pub struct SignInConfigBuilder(SignInConfig);

impl SignInConfigBuilder {
    pub fn scope(mut self, scope: impl Into<String>) -> Self {
        self.0.scopes.push(scope.into());
        self
    }

    pub fn scopes<I: IntoIterator<Item = S>, S: Into<String>>(mut self, scopes: I) -> Self {
        self.0.scopes.extend(scopes.into_iter().map(Into::into));
        self
    }

    pub fn hosted_domain(mut self, domain: impl Into<String>) -> Self {
        self.0.hosted_domain = Some(domain.into());
        self
    }

    pub fn nonce(mut self, nonce: impl Into<String>) -> Self {
        self.0.nonce = Some(nonce.into());
        self
    }

    pub const fn auto_select(mut self, auto_select: bool) -> Self {
        self.0.auto_select = auto_select;
        self
    }

    #[must_use]
    pub fn build(self) -> SignInConfig {
        self.0
    }
}

impl std::fmt::Display for SignInError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UserCancelled => f.write_str("user cancelled sign-in"),
            Self::NoCredentialAvailable => f.write_str("no credential available"),
            Self::Reauthenticate => f.write_str("re-authentication required"),
            Self::InvalidConfiguration(msg) => write!(f, "invalid sign-in configuration: {msg}"),
            Self::Network(msg) => write!(f, "network error during sign-in: {msg}"),
            Self::Backend(msg) => write!(f, "sign-in backend error: {msg}"),
        }
    }
}

impl std::error::Error for SignInError {}

#[plugin(
    name = "istmo.google_sign_in",
    init = SignInConfig,
    crate = "::istmo_core",
)]
pub trait SignIn {
    #[owned]
    async fn sign_in(&self, mode: SignInMode) -> Result<SignInAccount, SignInError>;

    #[owned]
    async fn silent_sign_in(&self) -> Result<Option<SignInAccount>, SignInError>;

    #[owned]
    async fn refresh(&self, credential: NativeHandleId) -> Result<SignInAccount, SignInError>;

    async fn sign_out(&self) -> Result<(), SignInError>;

    async fn revoke(&self) -> Result<(), SignInError>;
}
