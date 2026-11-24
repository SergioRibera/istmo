//! `AuthMode::Embedded` on desktop: an `istmo-webview` window whose
//! rules intercept the redirect URI.

use std::future::Future;
use std::pin::pin;
use std::sync::Arc;
use std::task::Poll;

use istmo_core::{CancelToken, Runtime};
use istmo_webview::{
    ControlEvent, PersistentProfile, UrlPattern, ViewEvent, WebProfile, WebViewClient,
    WebViewClientExt, WebViewError, WebViewInitConfig, WebViewSpec, WebViewSpecExt,
};
use url::Url;

use super::Deadline;
use crate::{AuthCallback, AuthRequest, VerifiedCallback, WebAuthError};

/// Profile non-ephemeral embedded sign-ins share, so a second sign-in
/// can reuse the provider session.
const SHARED_PROFILE: &str = "istmo.web_auth";

fn view_error(err: WebViewError) -> WebAuthError {
    match err {
        WebViewError::RenderModeUnsupported | WebViewError::Unsupported(_) => {
            WebAuthError::Unsupported(format!("embedded sign-in: {err}"))
        }
        WebViewError::RuntimeMissing(_) => WebAuthError::NoBrowser,
        other => WebAuthError::Backend(other.to_string()),
    }
}

fn redirect_pattern(redirect: &str) -> Result<UrlPattern, WebAuthError> {
    let url =
        Url::parse(redirect).map_err(|_| WebAuthError::InvalidArgument("redirect URI".into()))?;
    let host = url.host_str().filter(|h| !h.is_empty()).unwrap_or("*");
    let mut pattern = UrlPattern::host(url.scheme(), host).with_path(url.path());
    if let Some(port) = url.port() {
        pattern = pattern.with_port(port);
    }
    Ok(pattern)
}

/// Resolves when `cancel` fires or `deadline` passes.
fn stop_signal(cancel: &CancelToken, deadline: Deadline) -> CancelToken {
    let stop = CancelToken::new();
    let (outer, inner) = (cancel.clone(), stop.clone());
    let spawned = std::thread::Builder::new()
        .name("istmo-web-auth-deadline".into())
        .spawn(move || {
            loop {
                if outer.is_cancelled() || deadline.expired() || inner.is_cancelled() {
                    inner.cancel();
                    return;
                }
                std::thread::sleep(deadline.remaining(std::time::Duration::from_millis(100)));
            }
        });
    if spawned.is_err() {
        // Without the watcher only the view's own events end the wait.
        tracing::warn!("could not spawn the sign-in deadline watcher");
    }
    stop
}

/// First of `a` (an event) or `b` (stop).
async fn race<A: Future>(a: A, b: impl Future<Output = ()>) -> Option<A::Output> {
    let mut a = pin!(a);
    let mut b = pin!(b);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(out) = a.as_mut().poll(cx) {
            return Poll::Ready(Some(out));
        }
        b.as_mut().poll(cx).map(|()| None)
    })
    .await
}

pub(super) async fn run(
    runtime: &Arc<Runtime>,
    request: &AuthRequest,
    redirect: &str,
    cancel: &CancelToken,
    deadline: Deadline,
) -> Result<AuthCallback, WebAuthError> {
    let client = Arc::new(
        WebViewClient::from_runtime_with(runtime, WebViewInitConfig::default())
            .await
            .map_err(|err| view_error(WebViewError::from(err)))?,
    );
    let mut spec = WebViewSpec::window("Sign in", 480, 720).sign_in(
        redirect_pattern(redirect)?,
        // Providers hop across their own https hosts (and federated
        // identity providers); only the redirect URI is captured.
        [UrlPattern::host("https", "*")],
    );
    if !request.ephemeral {
        spec.profile = WebProfile::Persistent(PersistentProfile {
            name: SHARED_PROFILE.into(),
        });
    }
    let view = client.open_view(spec).await.map_err(view_error)?;
    let events = view
        .control()
        .ok_or_else(|| WebAuthError::Backend("no control lane".into()))?;
    view.load_url(request.authorize_url.clone())
        .await
        .map_err(view_error)?;

    let stop = stop_signal(cancel, deadline);
    let outcome = loop {
        let Some(event) = race(events.next(), stop.cancelled()).await else {
            break Err(if cancel.is_cancelled() {
                WebAuthError::Cancelled
            } else {
                WebAuthError::Timeout
            });
        };
        match event {
            Some(ViewEvent::Other(ControlEvent::Intercepted(nav))) => {
                if VerifiedCallback::verify(&nav.url, &request.expected_state, None).is_ok() {
                    break Ok(AuthCallback { url: nav.url });
                }
                // A redirect with someone else's state: ignore it.
            }
            Some(ViewEvent::Other(ControlEvent::CloseRequested)) | None => {
                break Err(WebAuthError::Cancelled);
            }
            Some(ViewEvent::Other(ControlEvent::ProcessTerminated(_))) => {
                break Err(WebAuthError::Backend("the sign-in page crashed".into()));
            }
            // Pending requests answer their safe default when dropped.
            Some(_) => {}
        }
    };
    stop.cancel();
    if let Err(err) = view.close().await {
        tracing::debug!(%err, "closing the sign-in view");
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redirect_patterns() {
        let p = redirect_pattern("http://127.0.0.1:51234/cb").unwrap();
        assert_eq!(
            (p.host.as_str(), p.port, p.path_prefix.as_str()),
            ("127.0.0.1", Some(51234), "/cb")
        );
        let p = redirect_pattern("com.example.app:/oauth").unwrap();
        assert_eq!(
            (p.scheme.as_str(), p.host.as_str()),
            ("com.example.app", "*")
        );
    }
}
