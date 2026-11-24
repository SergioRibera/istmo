//! Ergonomic client: [`SignInFlow`] holds everything one sign-in needs.

use std::time::Duration;

use crate::pkce::{Pkce, random_token};
use crate::{
    AuthMode, AuthRequest, CallbackSpec, PreparedRedirect, VerifiedCallback, WebAuthClient,
    WebAuthError,
};

/// How [`SignInFlow::complete`] shows the page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInOptions {
    pub mode: AuthMode,
    /// Do not share cookies with the user's browser. Off by default so
    /// single sign-on works; turn it on for "use another account".
    pub ephemeral: bool,
    /// `None` = wait until the user finishes or cancels.
    pub timeout: Option<Duration>,
    /// Issuer the callback's `iss` must equal, when it carries one.
    pub expected_issuer: Option<String>,
}

impl Default for SignInOptions {
    fn default() -> Self {
        Self {
            mode: AuthMode::System,
            ephemeral: false,
            timeout: Some(Duration::from_secs(600)),
            expected_issuer: None,
        }
    }
}

/// A prepared sign-in: the redirect is reserved and the PKCE pair,
/// `state` and `nonce` are minted. Build the authorization URL from it,
/// then [`complete`](Self::complete).
pub struct SignInFlow<'c> {
    client: &'c WebAuthClient,
    redirect: PreparedRedirect,
    pkce: Pkce,
    state: String,
    nonce: String,
}

impl std::fmt::Debug for SignInFlow<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SignInFlow")
            .field("redirect_uri", &self.redirect.redirect_uri)
            .finish_non_exhaustive()
    }
}

/// Extension trait giving [`WebAuthClient`] the sign-in flow.
pub trait WebAuthClientExt {
    /// Reserve `callback` and mint the flow's secrets.
    ///
    /// # Errors
    /// [`WebAuthError`] from the backend (an unsupported callback kind,
    /// no port to listen on).
    fn begin(
        &self,
        callback: CallbackSpec,
    ) -> impl std::future::Future<Output = Result<SignInFlow<'_>, WebAuthError>> + Send;
}

impl WebAuthClientExt for WebAuthClient {
    async fn begin(&self, callback: CallbackSpec) -> Result<SignInFlow<'_>, WebAuthError> {
        let redirect = self.prepare(callback).await.map_err(WebAuthError::from)?;
        Ok(SignInFlow {
            client: self,
            redirect,
            pkce: Pkce::generate(),
            state: random_token(24),
            nonce: random_token(24),
        })
    }
}

impl SignInFlow<'_> {
    /// `redirect_uri` exactly as it must be registered and sent.
    #[must_use]
    pub fn redirect_uri(&self) -> &str {
        &self.redirect.redirect_uri
    }

    /// [`Self::redirect_uri`] form-encoded for a query string.
    #[must_use]
    pub fn redirect_uri_encoded(&self) -> String {
        url::form_urlencoded::byte_serialize(self.redirect.redirect_uri.as_bytes()).collect()
    }

    /// PKCE pair: send the challenge now, the verifier with the token
    /// request.
    #[must_use]
    pub const fn pkce(&self) -> &Pkce {
        &self.pkce
    }

    /// `state` to send (URL-safe, no encoding needed).
    #[must_use]
    pub fn state(&self) -> &str {
        &self.state
    }

    /// `nonce` to send (OIDC); compare it with the ID token's claim.
    #[must_use]
    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    /// Open `authorize_url` and wait for the verified callback.
    ///
    /// # Errors
    /// [`WebAuthError::Cancelled`] when the user closes the browser,
    /// [`WebAuthError::Timeout`], [`WebAuthError::StateMismatch`] /
    /// [`WebAuthError::IssuerMismatch`], or a backend failure.
    pub async fn complete(
        &self,
        authorize_url: impl Into<String>,
        options: SignInOptions,
    ) -> Result<VerifiedCallback, WebAuthError> {
        let authorize_url = authorize_url.into();
        if !authorize_url.starts_with("https://") {
            return Err(WebAuthError::InvalidArgument(
                "authorize_url must be https".into(),
            ));
        }
        let timeout_ms = options.timeout.map_or(0, |t| {
            u32::try_from(t.as_millis()).unwrap_or(u32::MAX).max(1)
        });
        let request = AuthRequest {
            session: self.redirect.session,
            authorize_url,
            expected_state: self.state.clone(),
            mode: options.mode,
            ephemeral: options.ephemeral,
            timeout_ms,
        };
        let callback = self
            .client
            .authenticate(request)
            .await
            .map_err(WebAuthError::from)?;
        VerifiedCallback::verify(
            &callback.url,
            &self.state,
            options.expected_issuer.as_deref(),
        )
    }
}
