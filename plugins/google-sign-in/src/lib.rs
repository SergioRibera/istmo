//! Google Sign-In / Credential Manager plugin.
//!
//! Community-grade plugin distributed independently of the framework core.
//! Serves as a reference for how to structure an istmo plugin crate:
//!
//! * Declarative metadata in `istmo.toml` (permissions, native deps).
//! * `build.rs` one-liner (`emit_manifest_metadata`) forwards it to
//!   consumer apps through Cargo's cross-crate metadata channel.
//! * Wire types and dispatch trait live behind a single `#[istmo::plugin]`
//!   declaration.
//! * The `Contract` builder in [`codegen`] is feature-gated so the runtime
//!   dependency tree stays minimal — downstream apps flip the feature on
//!   inside `[build-dependencies]` when they need to codegen Kotlin / Swift
//!   host classes.
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

use std::sync::Arc;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId, Runtime};
use istmo_macros::plugin;

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

// `SignInMode`, `SignInConfig`, `SignInAccount`, `SignInError` are
// generated from the canonical `Contract` builder in `codegen::contract`
// via `build.rs`.
include!(concat!(env!("OUT_DIR"), "/google_sign_in_types.rs"));

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

/// Rust-owned counterpart of [`SignInAccount`]: same data, but `credential`
/// is an owned [`NativeHandle<Credential>`] that releases the native object
/// on drop.
///
/// Returned by [`SignInClient::sign_in_owned`] — the ergonomic entry point
/// that ties the credential's lifetime to Rust. Users that need to hand the
/// raw id back into a subsequent `SignIn` call (e.g. a refresh flow) go
/// through [`SignIn::sign_in`] instead and adopt manually when convenient.
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
