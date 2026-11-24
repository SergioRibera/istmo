//! App-scheme callback: the browser hands `scheme://host/path?…` to the
//! OS, which starts (or forwards to) the app; `DocumentsPublisher`
//! publishes it on `istmo.deeplinks`.

use std::sync::Arc;
use std::time::Duration;

use istmo_core::{CancelToken, Runtime};
use istmo_plugins::{DeepLinkStream, DeepLinks};
use url::Url;

use super::{Deadline, check};
use crate::{SchemeCallback, VerifiedCallback, WebAuthError};

const POLL: Duration = Duration::from_millis(50);

/// The callback a sign-in waits for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Target {
    scheme: String,
    host: String,
    path: String,
}

impl Target {
    pub(super) fn new(spec: &SchemeCallback) -> Result<Self, WebAuthError> {
        let scheme = spec.scheme.to_ascii_lowercase();
        let valid = scheme
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_lowercase())
            && scheme
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
            && !matches!(
                scheme.as_str(),
                "http" | "https" | "file" | "javascript" | "data"
            );
        let path = if spec.path.is_empty() {
            "/"
        } else {
            spec.path.as_str()
        };
        if !valid || !path.starts_with('/') || path.contains(['?', '#']) {
            return Err(WebAuthError::InvalidArgument(format!(
                "callback `{}://{}{}`",
                spec.scheme, spec.host, spec.path
            )));
        }
        Ok(Self {
            scheme,
            host: spec.host.to_ascii_lowercase(),
            path: path.to_owned(),
        })
    }

    pub(super) fn redirect_uri(&self) -> String {
        if self.host.is_empty() {
            format!("{}:{}", self.scheme, self.path)
        } else {
            format!("{}://{}{}", self.scheme, self.host, self.path)
        }
    }

    pub(super) fn subscribe(runtime: &Arc<Runtime>) -> Result<DeepLinkStream, WebAuthError> {
        DeepLinks::from_runtime(runtime)
            .map(|links| links.stream())
            .map_err(|err| {
                WebAuthError::Backend(format!(
                    "app-scheme callbacks need the `istmo.deeplinks` plugin: {err}"
                ))
            })
    }

    fn matches(&self, url: &Url) -> bool {
        url.scheme() == self.scheme
            && url
                .host_str()
                .unwrap_or_default()
                .eq_ignore_ascii_case(&self.host)
            && (url.path() == self.path || (self.path == "/" && url.path().is_empty()))
    }

    /// Read deep links until one is this callback with `state`.
    pub(super) fn wait(
        &self,
        links: &DeepLinkStream,
        state: &str,
        cancel: &CancelToken,
        deadline: Deadline,
    ) -> Result<String, WebAuthError> {
        loop {
            check(cancel, deadline)?;
            match links.try_recv() {
                Some(Ok(link)) => {
                    let Ok(url) = Url::parse(&link.uri) else {
                        continue;
                    };
                    if self.matches(&url)
                        && VerifiedCallback::verify(&link.uri, state, None).is_ok()
                    {
                        return Ok(link.uri);
                    }
                    // Not ours (another link, a stale callback): leave it.
                }
                Some(Err(err)) => return Err(WebAuthError::Backend(err.to_string())),
                None => std::thread::sleep(deadline.remaining(POLL)),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(scheme: &str, host: &str, path: &str) -> Result<Target, WebAuthError> {
        Target::new(&SchemeCallback {
            scheme: scheme.into(),
            host: host.into(),
            path: path.into(),
        })
    }

    #[test]
    fn redirect_uris_and_matching() {
        let t = target("com.example.app", "", "/oauth").unwrap();
        assert_eq!(t.redirect_uri(), "com.example.app:/oauth");
        assert!(t.matches(&Url::parse("com.example.app:/oauth?code=1").unwrap()));
        assert!(!t.matches(&Url::parse("com.example.app:/other").unwrap()));
        let t = target("MyApp", "auth", "").unwrap();
        assert_eq!(t.redirect_uri(), "myapp://auth/");
        assert!(t.matches(&Url::parse("myapp://auth?code=1").unwrap()));
        assert!(!t.matches(&Url::parse("myapp://evil/?code=1").unwrap()));
        for (s, p) in [
            ("https", "/x"),
            ("1app", "/x"),
            ("app", "x"),
            ("app", "/x?y"),
        ] {
            assert!(target(s, "h", p).is_err(), "{s} {p}");
        }
    }

    #[test]
    fn only_matching_link_with_state_is_taken() {
        let rt = Runtime::mock().runtime;
        rt.declare_plugin(istmo_plugins::DEEPLINKS_PLUGIN_ID);
        let t = target("myapp", "auth", "/cb").unwrap();
        let links = Target::subscribe(&rt).unwrap();
        for uri in [
            "myapp://auth/cb?code=1&state=old",
            "other://x",
            "myapp://auth/cb?code=2&state=s",
        ] {
            DeepLinks::publish(
                &rt,
                &istmo_plugins::DeepLink {
                    uri: uri.into(),
                    source: None,
                    received_at_ms: None,
                },
            )
            .unwrap();
        }
        let got = t
            .wait(&links, "s", &CancelToken::new(), Deadline::new(5_000))
            .unwrap();
        assert_eq!(got, "myapp://auth/cb?code=2&state=s");
    }
}
