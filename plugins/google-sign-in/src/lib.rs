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

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Copy)]
pub enum SignInMode {
    Interactive,
    SilentOnly,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignInConfig {
    pub server_client_id: String,
    pub scopes: Vec<String>,
    pub hosted_domain: Option<String>,
    pub nonce: Option<String>,
    pub auto_select: bool,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignInAccount {
    pub id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub photo_url: Option<String>,
    pub id_token: String,
    pub granted_scopes: Vec<String>,
    #[handle(Credential)]
    pub credential: NativeHandleId,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SignInError {
    UserCancelled,
    NoCredentialAvailable,
    Reauthenticate,
    InvalidConfiguration(String),
    Network(String),
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

