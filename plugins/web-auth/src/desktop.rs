//! Desktop host: the default browser plus a loopback listener or the
//! app's own URL scheme for the callback, or an `istmo-webview` window
//! (`embedded` feature).
//!
//! ```ignore
//! use istmo_web_auth::{WebAuthHost, desktop::DesktopWebAuth};
//! runtime.register_host(WebAuthHost::new(DesktopWebAuth::new(&runtime)));
//! ```
//!
//! App-scheme callbacks arrive through `istmo.deeplinks`: the app must
//! run `DocumentsPublisher` (which publishes command-line and Apple
//! Event URLs as deep links) and its package must register the scheme.

#[cfg(feature = "embedded")]
mod embedded;
mod loopback;
mod scheme;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use istmo_core::sync::lock;
use istmo_core::{CancelToken, Runtime};
use istmo_opener::{DesktopOpener, Opener, SchemePolicy};

use crate::pkce::random_u64;
use crate::{
    AuthCallback, AuthMode, AuthRequest, CallbackSpec, PreparedRedirect, WebAuth,
    WebAuthCapabilities, WebAuthError,
};

/// A reserved callback.
#[derive(Debug)]
enum Prepared {
    Loopback(loopback::Listener),
    Scheme(scheme::Target),
}

/// Rust host for Linux, macOS and Windows.
#[derive(Debug)]
pub struct DesktopWebAuth {
    runtime: Weak<Runtime>,
    sessions: Mutex<HashMap<u64, Prepared>>,
    busy: AtomicBool,
    launcher: Launcher,
}

/// Opens the authorization page.
pub type LaunchFn = Arc<dyn Fn(&str) -> Result<(), WebAuthError> + Send + Sync>;

enum Launcher {
    System(DesktopOpener),
    Custom(LaunchFn),
}

impl std::fmt::Debug for Launcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::System(_) => "Launcher::System",
            Self::Custom(_) => "Launcher::Custom",
        })
    }
}

impl DesktopWebAuth {
    /// Host serving `runtime` (deep links and embedded views are read
    /// from it).
    #[must_use]
    pub fn new(runtime: &Arc<Runtime>) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            sessions: Mutex::default(),
            busy: AtomicBool::new(false),
            // The authorization page is https; nothing else leaves.
            launcher: Launcher::System(DesktopOpener::with_policy(SchemePolicy::only(["https"]))),
        }
    }

    /// Open authorization pages with `launch` instead of the default
    /// browser (a specific browser, a private window, tests).
    #[must_use]
    pub fn with_launcher(mut self, launch: LaunchFn) -> Self {
        self.launcher = Launcher::Custom(launch);
        self
    }

    async fn launch(&self, url: &str) -> Result<(), WebAuthError> {
        match &self.launcher {
            Launcher::Custom(launch) => launch(url),
            Launcher::System(opener) => {
                opener.open(url.to_owned()).await.map_err(|err| match err {
                    istmo_opener::OpenError::NoHandler => WebAuthError::NoBrowser,
                    other => WebAuthError::Backend(other.to_string()),
                })
            }
        }
    }

    fn runtime(&self) -> Result<Arc<Runtime>, WebAuthError> {
        self.runtime
            .upgrade()
            .ok_or_else(|| WebAuthError::Backend("istmo runtime is gone".into()))
    }

    fn session_id(&self) -> u64 {
        let sessions = lock(&self.sessions);
        loop {
            let id = random_u64();
            if id != 0 && !sessions.contains_key(&id) {
                return id;
            }
        }
    }
}

/// Released by drop, so every exit path of `authenticate` frees it.
struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// When to stop waiting.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Deadline(Option<Instant>);

impl Deadline {
    fn new(timeout_ms: u32) -> Self {
        Self(
            (timeout_ms > 0)
                .then(|| Instant::now().checked_add(Duration::from_millis(timeout_ms.into())))
                .flatten(),
        )
    }

    pub(crate) fn expired(self) -> bool {
        self.0.is_some_and(|at| Instant::now() >= at)
    }

    /// Time left, capped at `cap`.
    pub(crate) fn remaining(self, cap: Duration) -> Duration {
        self.0.map_or(cap, |at| {
            at.saturating_duration_since(Instant::now()).min(cap)
        })
    }
}

/// Check the cancel token and the deadline.
pub(crate) fn check(cancel: &CancelToken, deadline: Deadline) -> Result<(), WebAuthError> {
    if cancel.is_cancelled() {
        Err(WebAuthError::Cancelled)
    } else if deadline.expired() {
        Err(WebAuthError::Timeout)
    } else {
        Ok(())
    }
}

impl WebAuth for DesktopWebAuth {
    async fn capabilities(&self) -> Result<WebAuthCapabilities, WebAuthError> {
        Ok(WebAuthCapabilities {
            backend: "system-browser".into(),
            loopback_callbacks: true,
            scheme_callbacks: true,
            https_callbacks: false,
            // The default browser keeps its own session; only the
            // embedded window can be ephemeral.
            ephemeral: false,
            embedded: cfg!(feature = "embedded"),
        })
    }

    async fn prepare(&self, callback: CallbackSpec) -> Result<PreparedRedirect, WebAuthError> {
        let (prepared, redirect_uri) = match callback {
            CallbackSpec::Loopback(spec) => {
                let listener = loopback::Listener::bind(&spec.path)?;
                let uri = listener.redirect_uri();
                (Prepared::Loopback(listener), uri)
            }
            CallbackSpec::CustomScheme(spec) => {
                let target = scheme::Target::new(&spec)?;
                let uri = target.redirect_uri();
                (Prepared::Scheme(target), uri)
            }
            CallbackSpec::Https(_) => {
                return Err(WebAuthError::Unsupported(
                    "https callbacks on desktop; use a loopback or app-scheme callback".into(),
                ));
            }
        };
        let session = self.session_id();
        lock(&self.sessions).insert(session, prepared);
        Ok(PreparedRedirect {
            redirect_uri,
            session,
        })
    }

    async fn authenticate(
        &self,
        request: AuthRequest,
        cancel: CancelToken,
    ) -> Result<AuthCallback, WebAuthError> {
        if self.busy.swap(true, Ordering::AcqRel) {
            return Err(WebAuthError::AlreadyInProgress);
        }
        let _busy = BusyGuard(&self.busy);
        let prepared = lock(&self.sessions)
            .remove(&request.session)
            .ok_or(WebAuthError::UnknownSession)?;
        let deadline = Deadline::new(request.timeout_ms);
        if request.mode == AuthMode::Embedded {
            return self.embedded(&request, &prepared, &cancel, deadline).await;
        }
        // Subscribe to deep links before the browser can answer.
        let pending = match &prepared {
            Prepared::Scheme(_) => Some(scheme::Target::subscribe(&self.runtime()?)?),
            Prepared::Loopback(_) => None,
        };
        self.launch(&request.authorize_url).await?;
        let url = match (prepared, pending) {
            (Prepared::Loopback(listener), _) => {
                listener.wait(&request.expected_state, &cancel, deadline)?
            }
            (Prepared::Scheme(target), Some(links)) => {
                target.wait(&links, &request.expected_state, &cancel, deadline)?
            }
            (Prepared::Scheme(_), None) => unreachable!("scheme sessions always subscribe"),
        };
        Ok(AuthCallback { url })
    }
}

impl DesktopWebAuth {
    #[cfg(not(feature = "embedded"))]
    #[expect(clippy::unused_async, reason = "same shape as the `embedded` variant")]
    async fn embedded(
        &self,
        _request: &AuthRequest,
        _prepared: &Prepared,
        _cancel: &CancelToken,
        _deadline: Deadline,
    ) -> Result<AuthCallback, WebAuthError> {
        Err(WebAuthError::Unsupported(
            "embedded sign-in needs the `embedded` feature".into(),
        ))
    }

    #[cfg(feature = "embedded")]
    async fn embedded(
        &self,
        request: &AuthRequest,
        prepared: &Prepared,
        cancel: &CancelToken,
        deadline: Deadline,
    ) -> Result<AuthCallback, WebAuthError> {
        let redirect = match prepared {
            Prepared::Loopback(listener) => listener.redirect_uri(),
            Prepared::Scheme(target) => target.redirect_uri(),
        };
        embedded::run(&self.runtime()?, request, &redirect, cancel, deadline).await
    }
}
