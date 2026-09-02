//! Google Sign-In / Credential Manager plugin.
//!
//! First real-world plugin bundled with the framework (milestone **M6**).
//! Validates three cross-cutting features of the runtime at once:
//!
//! * **`acquire_with`** — the plugin ships a [`SignInConfig`] to the native
//!   factory (server client id, requested scopes, …); the returned instance
//!   id is registered on the runtime and threaded into every subsequent
//!   call.
//! * **`NativeHandle`** — the native side owns a live
//!   `androidx.credentials.Credential` / `ASAuthorizationAppleIDCredential`
//!   / `GTMAppAuth` session; Rust references it through a
//!   [`NativeHandle<Credential>`] whose drop cleans up the backing object.
//!   Wire types carry a [`NativeHandleId`] (a plain `u64`), the client
//!   wrapper adopts it into an owned handle bound to the caller's runtime.
//! * **Native dependency merge** — the plugin declares the concrete Gradle
//!   coordinates and SPM products it needs (Credentials + Play Services
//!   Auth on Android, `GoogleSignIn-iOS` on iOS). Consumers aggregate them
//!   via [`istmo_build::NativeDeps`] into a single fragment.
//!
//! Wire identifier is `istmo.google_sign_in` under the `istmo.` core
//! namespace.

use std::sync::Arc;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId, Runtime};
use istmo_macros::{message, plugin};

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

/// Configuration handed to the native factory on [`SignInClient::acquire_with`].
///
/// `server_client_id` is required; every other field has a defaulting story
/// via [`SignInConfig::builder`]. The rationale for a small struct rather
/// than a fully-optional configuration DSL: sign-in setup is almost always
/// captured once at app startup, so the friction of naming every argument is
/// well-worth the readability at the call site.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInConfig {
    /// OAuth 2.0 client id registered for the *backend* — the identifier the
    /// id-token audience must match. Required.
    pub server_client_id: String,
    /// OAuth scopes to request beyond the default `openid profile email`.
    /// Order is preserved so the platform prompt shows them in the same
    /// sequence the caller listed.
    pub scopes: Vec<String>,
    /// `GSuite` domain restriction, e.g. `example.com`. `None` = no hosted
    /// domain restriction.
    pub hosted_domain: Option<String>,
    /// A cryptographically random nonce the caller expects back in the
    /// id-token's `nonce` claim. Recommended for replay-attack prevention;
    /// see Google's docs on ID Token verification.
    pub nonce: Option<String>,
    /// Whether the platform is allowed to auto-select the last-used account
    /// without user interaction (Android Credential Manager `autoSelect`).
    pub auto_select: bool,
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

/// UX shape requested at [`SignIn::sign_in`] time.
///
/// The plugin exposes a single `sign_in` method that both Android's
/// `CredentialManager.getCredential(...)` and iOS's `ASAuthorization`
/// implementations can service. `Interactive` shows the account picker;
/// `SilentOnly` fails fast with [`SignInError::NoCredentialAvailable`] when
/// no cached credential exists.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignInMode {
    /// Prompt the user if no credential can be resolved silently. This is
    /// the default for a normal sign-in button tap.
    Interactive,
    /// Never prompt — return `NoCredentialAvailable` if a fresh interactive
    /// flow would be required. Used for app-startup restoration.
    SilentOnly,
}

/// Wire-side view of a signed-in Google account.
///
/// `credential` is the numeric id under which the native side has parked the
/// concrete platform credential object. Client wrappers adopt it via
/// [`NativeHandle::adopt`] on the caller's runtime; see
/// [`SignInClient::sign_in`].
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInAccount {
    /// Google account id (the `sub` claim on the id-token).
    pub id: String,
    /// Email address, when the user granted the `email` scope.
    pub email: Option<String>,
    /// Display name, when the user granted the `profile` scope.
    pub display_name: Option<String>,
    /// Signed URL of the user's profile picture. `None` when the user has no
    /// picture or the `profile` scope was denied.
    pub photo_url: Option<String>,
    /// Signed id-token in JWT form. The caller verifies the signature and
    /// audience server-side; the plugin does not enforce it.
    pub id_token: String,
    /// Scopes actually granted, which may be a subset of the ones requested.
    pub granted_scopes: Vec<String>,
    /// Native handle id pointing at the platform-specific credential object.
    /// See [`Credential`].
    pub credential: NativeHandleId,
}

/// Rust-owned counterpart of [`SignInAccount`]: same data, but `credential`
/// is an owned [`NativeHandle<Credential>`] that releases the native object
/// on drop.
///
/// Returned by [`SignInClient::sign_in_owned`] — the ergonomic entry point
/// that ties the credential's lifetime to Rust. Users that need to hand the
/// raw id back into a subsequent `SignIn` call (e.g. a refresh flow) go
/// through [`SignInClient::sign_in`] instead and adopt manually when
/// convenient.
#[derive(Debug)]
pub struct OwnedSignInAccount {
    pub id: String,
    pub email: Option<String>,
    pub display_name: Option<String>,
    pub photo_url: Option<String>,
    pub id_token: String,
    pub granted_scopes: Vec<String>,
    pub credential: NativeHandle<Credential>,
}

/// Reasons a sign-in attempt could fail on the domain side.
///
/// `Backend` is the free-form escape hatch for anything the platform
/// surfaces that we did not translate into a typed variant — should be rare
/// once a real backend exists; kept so plugin adoption is not blocked on
/// exhaustive taxonomy work.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInError {
    /// The user closed / cancelled the account picker (Android
    /// `NoCredentialException` with `TYPE_USER_CANCELED`, iOS
    /// `ASAuthorizationError.canceled`).
    UserCancelled,
    /// No credential was available and the request was
    /// [`SignInMode::SilentOnly`], or Credential Manager reported no
    /// available accounts under interactive mode.
    NoCredentialAvailable,
    /// Refreshing the id-token requires user interaction that
    /// [`SignInMode::SilentOnly`] disallows.
    Reauthenticate,
    /// The requested configuration is invalid or missing platform setup
    /// (wrong `server_client_id`, unsigned SHA-1 mismatch, no
    /// GoogleService-Info.plist).
    InvalidConfiguration(String),
    /// Network failure (no connectivity, timeout, backend 5xx during token
    /// exchange).
    Network(String),
    /// Free-form platform message. Reserved for the long tail of exceptions
    /// the backend cannot classify — keep populating typed variants as they
    /// stabilise.
    Backend(String),
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

/// Google Sign-In / Credential Manager plugin surface.
#[plugin(
    name = "istmo.google_sign_in",
    init = SignInConfig,
    crate = "::istmo_core",
)]
pub trait SignIn {
    /// Requests a credential. `mode` chooses interactive vs silent-only
    /// behaviour; the returned account carries a [`NativeHandleId`] the
    /// caller adopts into a [`NativeHandle<Credential>`] via
    /// [`NativeHandle::adopt`] to tie the object's lifetime to Rust.
    async fn sign_in(&self, mode: SignInMode) -> Result<SignInAccount, SignInError>;

    /// Best-effort silent sign-in intended for app startup. Returns `None`
    /// when the platform reports no available credential — a normal state
    /// on first launch, deliberately distinguished from
    /// [`SignInError::NoCredentialAvailable`] which is reserved for
    /// [`sign_in`](Self::sign_in) with [`SignInMode::SilentOnly`].
    async fn silent_sign_in(&self) -> Result<Option<SignInAccount>, SignInError>;

    /// Refresh the id-token for the credential already parked under
    /// `credential`. The native side keeps the same handle id and returns a
    /// fresh id-token + `granted_scopes` snapshot.
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

impl SignInClient {
    /// Returns the runtime bound to this client — handy for adopting
    /// [`NativeHandleId`]s returned from [`SignIn::sign_in`] /
    /// [`SignIn::refresh`] into typed [`NativeHandle<Credential>`]s.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.__runtime
    }

    /// Ergonomic wrapper on top of the trait method that adopts the returned
    /// [`NativeHandleId`] into an owned [`NativeHandle<Credential>`] tied to
    /// this client's runtime.
    ///
    /// Prefer this over [`SignIn::sign_in`] when you want the credential's
    /// native lifetime to be managed by Rust — dropping the returned
    /// [`OwnedSignInAccount`] releases the native object.
    ///
    /// # Errors
    /// Same as [`SignIn::sign_in`].
    pub async fn sign_in_owned(&self, mode: SignInMode) -> Result<OwnedSignInAccount, IstmoError> {
        let acc = self.sign_in(mode).await?;
        Ok(adopt_account(&self.__runtime, acc))
    }

    /// Same as [`Self::sign_in_owned`] for the silent flow.
    pub async fn silent_sign_in_owned(&self) -> Result<Option<OwnedSignInAccount>, IstmoError> {
        Ok(self
            .silent_sign_in()
            .await?
            .map(|acc| adopt_account(&self.__runtime, acc)))
    }
}

fn adopt_account(rt: &Arc<Runtime>, wire: SignInAccount) -> OwnedSignInAccount {
    OwnedSignInAccount {
        id: wire.id,
        email: wire.email,
        display_name: wire.display_name,
        photo_url: wire.photo_url,
        id_token: wire.id_token,
        granted_scopes: wire.granted_scopes,
        credential: NativeHandle::adopt(rt, wire.credential),
    }
}
