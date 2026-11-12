//! Long-running background service control channel.
//!
//! The runtime exposes a [`ServiceControl`] client that platform code
//! can use to start, stop and re-enter a
//! [`#[istmo::service]`](../../../istmo_macros/attr.service.html)
//! adapter. The service impl receives a [`ServiceContext`] with a
//! [`StopNotifier`] and helper [`WakeLock`] handles.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use flume::{Receiver, Sender, bounded};
use istmo_core::{CancelToken, IstmoError, Runtime, codec};
use istmo_macros::{message, plugin};

/// Wire identifier for the service-control plugin.
pub const SERVICE_CONTROL_PLUGIN_ID: &str = "istmo.service_control";

/// Foreground-service notification shown while a service runs.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSpec {
    /// Android notification channel id.
    pub channel_id: String,
    /// Id the notification is posted (and later updated) under.
    pub notification_id: i32,
    /// Title line.
    pub title: String,
    /// Body text.
    pub body: String,
    /// Drawable resource name for the small icon; the app icon when
    /// `None`.
    pub small_icon: Option<String>,
    /// Non-dismissable while the service runs.
    pub ongoing: bool,
    /// Android `ServiceInfo.FOREGROUND_SERVICE_TYPE_*` bit set.
    pub foreground_service_type: Option<u32>,
}

/// Opaque id of a wake lock the platform holds for a service.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WakelockToken {
    /// Platform-assigned id.
    pub token: u64,
}

impl WakelockToken {
    /// Token with the platform-assigned id `token`.
    #[must_use]
    pub const fn new(token: u64) -> Self {
        Self { token }
    }
}

impl From<u64> for WakelockToken {
    fn from(token: u64) -> Self {
        Self::new(token)
    }
}

/// Failure reported by the platform's service-control host.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceControlError {
    /// No service is registered under this id.
    UnknownService(String),
    /// The platform refused the wake lock.
    WakelockDenied(String),
    /// Any other platform failure.
    Platform(String),
}

impl core::fmt::Display for ServiceControlError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::UnknownService(id) => write!(f, "unknown service `{id}`"),
            Self::WakelockDenied(msg) => write!(f, "wakelock denied: {msg}"),
            Self::Platform(msg) => write!(f, "service control platform error: {msg}"),
        }
    }
}

impl std::error::Error for ServiceControlError {}

/// Platform side of a running service: foreground notification, wake
/// locks and self-stop.
#[plugin(name = "istmo.service_control", crate = "::istmo_core")]
pub trait ServiceControl {
    /// Promote `service_id` to a foreground service showing `spec`.
    async fn start_foreground(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    /// Replace the foreground notification of `service_id`.
    async fn update_notification(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    /// Leave the foreground, optionally removing the notification.
    async fn stop_foreground(
        &self,
        service_id: String,
        remove_notification: bool,
    ) -> Result<(), ServiceControlError>;

    /// Acquire a partial wake lock tagged `tag` for `service_id`.
    async fn acquire_wakelock(
        &self,
        service_id: String,
        tag: String,
    ) -> Result<WakelockToken, ServiceControlError>;

    /// Release a wake lock from [`acquire_wakelock`](Self::acquire_wakelock).
    async fn release_wakelock(
        &self,
        service_id: String,
        token: WakelockToken,
    ) -> Result<(), ServiceControlError>;

    /// Stop the platform service hosting `service_id`.
    async fn stop_self(&self, service_id: String) -> Result<(), ServiceControlError>;
}

/// Sending half of a service's stop signal.
#[derive(Debug, Clone)]
pub struct StopNotifier {
    tx: Sender<()>,
}

impl StopNotifier {
    /// Ask the service to stop. Repeated signals collapse into one.
    pub fn signal(&self) {
        // A full channel already holds a pending stop.
        let _ = self.tx.try_send(());
    }
}

/// A stop signal pair: the notifier for the host, the receiver for the
/// [`ServiceContext`].
#[must_use]
pub fn stop_channel() -> (StopNotifier, Receiver<()>) {
    let (tx, rx) = bounded(1);
    (StopNotifier { tx }, rx)
}

/// Handed to a service's `on_start`: its identity, stop signal and the
/// calls it can make on its own platform service.
#[derive(Debug)]
pub struct ServiceContext {
    control: ServiceControlClient,
    service_id: String,
    stop_rx: Receiver<()>,
    stopped_flag: AtomicBool,
    cancel: CancelToken,
}

impl ServiceContext {
    /// Context for the service `service_id` running on `runtime`.
    ///
    /// Declares the service-control plugin on `runtime`: a running
    /// service implies the platform host that runs it.
    #[must_use]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "owned `Arc` is the signature `#[istmo::service]` expansions call"
    )]
    pub fn new(
        runtime: Arc<Runtime>,
        service_id: String,
        stop_rx: Receiver<()>,
        cancel: CancelToken,
    ) -> Self {
        runtime.declare_plugin(SERVICE_CONTROL_PLUGIN_ID);
        let control = ServiceControlClient::from_runtime(&runtime)
            .expect("invariant: the plugin was declared just above");
        Self {
            control,
            service_id,
            stop_rx,
            stopped_flag: AtomicBool::new(false),
            cancel,
        }
    }

    /// Cooperative cancellation for this run of the service.
    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    /// Id the service was registered under.
    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Runtime the service runs on.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        self.control.runtime()
    }

    /// Generated client for the service-control plugin.
    #[must_use]
    pub const fn control(&self) -> &ServiceControlClient {
        &self.control
    }

    /// Promote the service to the foreground with `spec` as its
    /// notification.
    ///
    /// # Errors
    ///
    /// Transport errors, or [`IstmoError::PluginError`] carrying a
    /// [`ServiceControlError`].
    pub async fn set_foreground(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        self.control
            .start_foreground(self.service_id.clone(), spec)
            .await
    }

    /// Replace the foreground notification.
    ///
    /// # Errors
    ///
    /// Same as [`set_foreground`](Self::set_foreground).
    pub async fn update_notification(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        self.control
            .update_notification(self.service_id.clone(), spec)
            .await
    }

    /// Leave the foreground, optionally removing the notification.
    ///
    /// # Errors
    ///
    /// Same as [`set_foreground`](Self::set_foreground).
    pub async fn stop_foreground(&self, remove_notification: bool) -> Result<(), IstmoError> {
        self.control
            .stop_foreground(self.service_id.clone(), remove_notification)
            .await
    }

    /// Acquire a wake lock tagged `tag`, released when the returned
    /// [`WakeLock`] is released or dropped.
    ///
    /// # Errors
    ///
    /// Same as [`set_foreground`](Self::set_foreground).
    pub async fn acquire_wakelock(&self, tag: &str) -> Result<WakeLock, IstmoError> {
        let token = self
            .control
            .acquire_wakelock(self.service_id.clone(), tag.to_owned())
            .await?;
        Ok(WakeLock {
            token,
            runtime: Arc::downgrade(self.runtime()),
            service_id: self.service_id.clone(),
        })
    }

    /// Ask the platform to stop this service.
    ///
    /// # Errors
    ///
    /// Same as [`set_foreground`](Self::set_foreground).
    pub async fn stop_self(&self) -> Result<(), IstmoError> {
        self.control.stop_self(self.service_id.clone()).await
    }

    /// Resolve once the service is asked to stop.
    pub async fn stopped(&self) {
        if self.is_stopped() {
            return;
        }
        // A disconnect means the host is gone, which is a stop too.
        let _ = self.stop_rx.recv_async().await;
        self.stopped_flag.store(true, Ordering::Release);
    }

    /// Whether a stop was signalled, the host went away or the run was
    /// cancelled.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        if self.stopped_flag.load(Ordering::Acquire) {
            return true;
        }
        if self.cancel.is_cancelled() || self.stop_rx.is_disconnected() || !self.stop_rx.is_empty()
        {
            self.stopped_flag.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

/// A held wake lock. Dropping it releases the lock with a
/// fire-and-forget notify; [`release`](Self::release) waits for the
/// platform instead.
#[derive(Debug)]
pub struct WakeLock {
    token: WakelockToken,
    runtime: Weak<Runtime>,
    service_id: String,
}

impl WakeLock {
    /// The platform's id for this lock.
    #[must_use]
    pub const fn token(&self) -> WakelockToken {
        self.token
    }

    /// Release the lock and wait for the platform to confirm.
    ///
    /// # Errors
    ///
    /// [`IstmoError::RuntimeNotStarted`] when the runtime is gone, else
    /// the errors of [`ServiceContext::set_foreground`].
    pub async fn release(mut self) -> Result<(), IstmoError> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or(IstmoError::RuntimeNotStarted)?;
        // Disarm `Drop` before the call so the lock is released once.
        self.runtime = Weak::new();
        let service_id = mem::take(&mut self.service_id);
        ServiceControlClient::from_runtime(&runtime)?
            .release_wakelock(service_id, self.token)
            .await
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let service_id = mem::take(&mut self.service_id);
        let payload = match codec::encode(&(service_id, self.token)) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(target: "istmo.service", %err, "wakelock release: encode failed");
                return;
            }
        };
        // Same method and argument tuple as the generated
        // `release_wakelock` call, sent as a notify because `Drop`
        // cannot wait for the response.
        if let Err(err) =
            runtime.notify(SERVICE_CONTROL_PLUGIN_ID, None, "release_wakelock", payload)
        {
            tracing::warn!(target: "istmo.service", %err, "wakelock release: notify submit failed");
        }
    }
}
