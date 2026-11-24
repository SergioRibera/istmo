//! Callback parsing and verification.

use url::Url;

use crate::WebAuthError;

/// `error` / `error_description` the provider put in the callback
/// (RFC 6749 §4.1.2.1): the user refused, the client is misconfigured…
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    pub error: String,
    pub description: Option<String>,
    pub uri: Option<String>,
}

/// A callback whose `state` (and `iss`, when expected) matched.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedCallback {
    url: String,
    params: Vec<(String, String)>,
}

impl std::fmt::Debug for VerifiedCallback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: Vec<&str> = self.params.iter().map(|(k, _)| k.as_str()).collect();
        f.debug_struct("VerifiedCallback")
            .field("params", &names)
            .finish_non_exhaustive()
    }
}

/// Parameters of a callback URL: the query, or the fragment when the
/// query carries none (`response_mode=fragment`).
///
/// # Errors
/// [`WebAuthError::InvalidArgument`] when `url` does not parse.
pub fn parse_params(url: &str) -> Result<Vec<(String, String)>, WebAuthError> {
    let parsed =
        Url::parse(url).map_err(|_| WebAuthError::InvalidArgument("callback URL".into()))?;
    let query: Vec<(String, String)> = parsed.query_pairs().into_owned().collect();
    if !query.is_empty() {
        return Ok(query);
    }
    Ok(parsed
        .fragment()
        .map(|fragment| {
            url::form_urlencoded::parse(fragment.as_bytes())
                .into_owned()
                .collect()
        })
        .unwrap_or_default())
}

/// Equal-length-independent constant-time comparison.
fn same(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = u8::from(a.len() != b.len());
    for (i, x) in a.iter().enumerate() {
        diff |= x ^ b.get(i).copied().unwrap_or(!x);
    }
    diff == 0
}

impl VerifiedCallback {
    /// Check `url` against the request's `state` and, when given, the
    /// issuer (RFC 9207 `iss`, sent by providers that advertise
    /// `authorization_response_iss_parameter_supported`).
    ///
    /// # Errors
    /// [`WebAuthError::StateMismatch`], [`WebAuthError::IssuerMismatch`],
    /// [`WebAuthError::InvalidArgument`] for a URL that does not parse.
    pub fn verify(
        url: &str,
        expected_state: &str,
        expected_issuer: Option<&str>,
    ) -> Result<Self, WebAuthError> {
        let params = parse_params(url)?;
        let get = |name: &str| {
            params
                .iter()
                .find(|(k, _)| k == name)
                .map(|(_, v)| v.as_str())
        };
        if !get("state").is_some_and(|state| same(state, expected_state)) {
            return Err(WebAuthError::StateMismatch);
        }
        if let Some(issuer) = expected_issuer
            && get("iss").is_some_and(|iss| iss != issuer)
        {
            return Err(WebAuthError::IssuerMismatch);
        }
        Ok(Self {
            url: url.to_owned(),
            params,
        })
    }

    /// The full callback URL.
    #[must_use]
    pub fn url(&self) -> &str {
        &self.url
    }

    /// First value of parameter `name`.
    #[must_use]
    pub fn param(&self, name: &str) -> Option<&str> {
        self.params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// Every parameter, in order.
    #[must_use]
    pub fn params(&self) -> &[(String, String)] {
        &self.params
    }

    /// The provider's refusal, when it sent one.
    #[must_use]
    pub fn provider_error(&self) -> Option<ProviderError> {
        self.param("error").map(|error| ProviderError {
            error: error.to_owned(),
            description: self.param("error_description").map(str::to_owned),
            uri: self.param("error_uri").map(str::to_owned),
        })
    }

    /// The authorization `code`.
    ///
    /// # Errors
    /// [`WebAuthError::MissingCode`] when the callback has none (check
    /// [`Self::provider_error`] for why).
    pub fn code(&self) -> Result<&str, WebAuthError> {
        self.param("code").ok_or(WebAuthError::MissingCode)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_state_and_issuer() {
        let ok = VerifiedCallback::verify(
            "http://127.0.0.1:5000/cb?code=abc&state=xyz&iss=https%3A%2F%2Fid.example",
            "xyz",
            Some("https://id.example"),
        )
        .unwrap();
        assert_eq!(ok.code().unwrap(), "abc");
        assert!(ok.provider_error().is_none());
        assert!(!format!("{ok:?}").contains("abc"), "debug hides values");

        let mismatch = VerifiedCallback::verify("app:/cb?code=a&state=other", "xyz", None);
        assert_eq!(mismatch.unwrap_err(), WebAuthError::StateMismatch);
        let missing = VerifiedCallback::verify("app:/cb?code=a", "xyz", None);
        assert_eq!(missing.unwrap_err(), WebAuthError::StateMismatch);
        let prefix = VerifiedCallback::verify("app:/cb?code=a&state=xy", "xyz", None);
        assert_eq!(prefix.unwrap_err(), WebAuthError::StateMismatch);
        let iss = VerifiedCallback::verify(
            "app:/cb?code=a&state=xyz&iss=https://evil",
            "xyz",
            Some("https://id.example"),
        );
        assert_eq!(iss.unwrap_err(), WebAuthError::IssuerMismatch);
    }

    #[test]
    fn reads_fragment_and_provider_errors() {
        let cb = VerifiedCallback::verify(
            "app:/cb#state=s&error=access_denied&error_description=no",
            "s",
            None,
        )
        .unwrap();
        let err = cb.provider_error().unwrap();
        assert_eq!(err.error, "access_denied");
        assert_eq!(err.description.as_deref(), Some("no"));
        assert_eq!(cb.code().unwrap_err(), WebAuthError::MissingCode);
    }
}
