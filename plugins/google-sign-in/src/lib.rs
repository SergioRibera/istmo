//! Google Sign-In / Credential Manager plugin.
//!
//! Community-grade plugin distributed independently of the framework core.
//! Serves as a reference for how to structure an istmo plugin crate:
//!
//! * Declarative metadata in `istmo.toml` (permissions, native deps).
//! * `build.rs` one-liner (`emit_manifest_metadata`) forwards it to
//!   consumer apps through Cargo's cross-crate metadata channel.
//! * Wire types and dispatch trait live behind a single `#[istmo::plugin]`
//!   declaration; the [`Contract`] is derived from that same source at
//!   `build.rs` time via [`istmo_build::extract_contract`].
//! * The [`codegen`] module (feature-gated) re-exports the extracted
//!   contract so downstream apps can codegen Kotlin / Swift host classes
//!   without going through the `DEP_*_ISTMO_CONTRACT` env handover.
//!
//! [`Contract`]: istmo_build::Contract
//!
//! Validates three cross-cutting features of the runtime:
//!
//! * **`acquire_with`** — the plugin ships a [`SignInConfig`] to the native
//!   factory (server client id, requested scopes, …); the returned instance
//!   id is registered on the runtime and threaded into every subsequent
//!   call.
//! * **`NativeHandle`** — the native side owns a live
//!   `androidx.credentials.Credential` /
//!   `ASAuthorizationAppleIDCredential` / `GTMAppAuth` session; Rust
//!   references it through a [`NativeHandle<Credential>`] whose drop cleans
//!   up the backing object.
//! * **Native dependency merge** — the plugin declares the concrete Gradle
//!   coordinates and SPM products it needs in `istmo.toml`. Consumers
//!   aggregate them via [`istmo_build::NativeDeps`] into a single fragment.
//!
//! Wire identifier is `istmo.google_sign_in` under the `istmo.` core
//! namespace.

#[cfg(feature = "codegen")]
pub mod codegen;

use istmo_core::NativeHandleId;
use istmo_macros::{message, owned, plugin};

/// Wire identifier of the sign-in plugin.
pub const GOOGLE_SIGN_IN_PLUGIN_ID: &str = "istmo.google_sign_in";

/// Marker type used as the phantom parameter of the credential handle. Only
/// its identity matters — the value is never constructed.
///
/// A `NativeHandle<Credential>` refers to whichever platform object is the
/// authoritative source for the signed-in user: an
/// `androidx.credentials.Credential` on Android, an
/// `ASAuthorizationAppleIDCredential` or a `GTMAppAuth` session on iOS. The
/// wrapper trait leaves the shape opaque so future backends (Passkeys,
/// custom `OpenID` providers) do not need a new type.
#[non_exhaustive]
#[derive(Debug)]
pub enum Credential {}

// Wire shape lives here as the single source of truth. `build.rs` calls
// [`istmo_build::extract_contract`] over this file to derive the `Contract`
// consumed by downstream Kotlin / Swift generators.

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
    /// Fluent builder starting from a required server client id.
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

/// Fluent builder for [`SignInConfig`]. Newtype rather than exposing
/// `&mut SignInConfig` so the shape can grow (extra scopes, sign-in-with
/// -Apple bridging, …) without breaking callers.
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

// `OwnedSignInAccount` + `SignInAccount::into_owned` are emitted by the
// `#[message]` macro from the `#[handle(Credential)]` field annotation
// above. Dropping the owned wrapper fires `Frame::ReleaseNativeHandle` on
// the native side.

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

/// Google Sign-In / Credential Manager plugin surface.
#[plugin(
    name = "istmo.google_sign_in",
    init = SignInConfig,
    crate = "::istmo_core",
)]
pub trait SignIn {
    /// Requests a credential. `mode` chooses interactive vs silent-only
    /// behaviour; the returned account carries a [`NativeHandleId`] the
    /// caller adopts into a [`NativeHandle`](istmo_core::NativeHandle)
    /// via [`sign_in_owned`](SignInClient::sign_in_owned) (auto-emitted
    /// from the `#[owned]` marker below) or manually via
    /// [`SignInAccount::into_owned`].
    #[owned]
    async fn sign_in(&self, mode: SignInMode) -> Result<SignInAccount, SignInError>;

    /// Best-effort silent sign-in intended for app startup. Returns `None`
    /// when the platform reports no available credential — a normal state
    /// on first launch, deliberately distinguished from
    /// [`SignInError::NoCredentialAvailable`] which is reserved for
    /// [`sign_in`](Self::sign_in) with [`SignInMode::SilentOnly`].
    #[owned]
    async fn silent_sign_in(&self) -> Result<Option<SignInAccount>, SignInError>;

    /// Refresh the id-token for the credential already parked under
    /// `credential`. The native side keeps the same handle id and returns a
    /// fresh id-token + `granted_scopes` snapshot.
    #[owned]
    async fn refresh(&self, credential: NativeHandleId) -> Result<SignInAccount, SignInError>;

    /// Signs the user out on the platform, invalidating cached credentials.
    /// The `credential` handle is expected to be released on the Rust side
    /// separately (typically by dropping its `NativeHandle<Credential>`).
    async fn sign_out(&self) -> Result<(), SignInError>;

    /// Revokes the OAuth grant entirely (Android's
    /// `GoogleSignIn.getClient().revokeAccess()`, iOS's `OpenID`
    /// `revokeToken`). Distinct from sign-out because it affects future
    /// requests to the backend.
    async fn revoke(&self) -> Result<(), SignInError>;
}

// `SignInClient::{sign_in_owned, silent_sign_in_owned, refresh_owned}` are
// emitted by the `#[plugin]` macro from the `#[owned]` markers on the
// trait methods above.
