//! Background-service primitives.
//!
//! A "service" in istmo terminology is a Rust trait implementation whose
//! lifecycle is driven by the platform (Android `LifecycleService`, iOS
//! `BGProcessingTask` shim, or a desktop mock). The platform issues an
//! `on_start` call carrying a [`ServiceContext`]; the Rust side runs its
//! long-lived task until either the platform signals shutdown or the impl
//! decides to stop itself.
//!
//! Two surfaces live here:
//!
//! * [`ServiceContext`] — passed to the service impl on `on_start`. Wraps
//!   the runtime and exposes foreground-notification, wakelock and
//!   stop-signal helpers.
//! * [`ServiceControl`] — the plugin trait the platform implements to
//!   service those helper calls. `#[istmo::service]` generates a service
//!   adapter that constructs the [`ServiceContext`]; the adapter itself
//!   hosts the `on_start`/`on_stop` dispatch and lives in the codegen layer.
//!
//! The plugin trait exists so the codegen path is exercised end-to-end from
//! Slice A. Real Android impls will be wired in Slice D; desktop mocks (and
//! this crate's tests) provide a hand-written [`ServiceControl`] impl.

use std::mem;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use flume::{Receiver, Sender, bounded};
use istmo_core::{IstmoError, Runtime, codec};
use istmo_macros::{message, plugin};

/// Wire identifier of the service-control plugin.
pub const SERVICE_CONTROL_PLUGIN_ID: &str = "istmo.service_control";

/// Descriptor of a foreground notification.
///
/// Fields map onto the union of Android's `Notification.Builder` and iOS's
/// `UNMutableNotificationContent`. Backends ignore fields that do not apply
/// to their platform.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSpec {
    /// Platform notification channel id (Android O+). Required on Android;
    /// ignored on iOS.
    pub channel_id: String,
    /// Numeric id used by `startForeground(id, notification)` on Android and
    /// by iOS to identify the notification for later updates.
    pub notification_id: i32,
    /// Notification title displayed in the shade.
    pub title: String,
    /// Notification body / secondary line.
    pub body: String,
    /// Optional small-icon resource name (Android drawable identifier or
    /// iOS `UNNotificationAttachment` identifier). `None` uses the app's
    /// default launcher icon.
    pub small_icon: Option<String>,
    /// Whether the notification is ongoing (non-dismissable while the
    /// service runs). Foreground services usually set this to `true`.
    pub ongoing: bool,
    /// Bitmask of Android `FOREGROUND_SERVICE_TYPE_*` constants declared for
    /// the service. Required on API 34+; `None` leaves the backend to fall
    /// back to `FOREGROUND_SERVICE_TYPE_NONE`.
    pub foreground_service_type: Option<u32>,
}

/// Opaque token identifying a wakelock previously acquired via
/// [`ServiceContext::acquire_wakelock`]. The token is meaningful to the
/// platform backend only.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WakelockToken(pub u64);

/// Domain errors surfaced by the service-control plugin. Transport failures
/// stay in [`IstmoError`]; this enum only covers cases the backend can
/// legitimately report at the domain level.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceControlError {
    /// The referenced `service_id` is not registered with the backend.
    UnknownService(String),
    /// The wakelock request was denied by the platform (e.g. missing
    /// `WAKE_LOCK` permission on Android).
    WakelockDenied(String),
    /// Free-form platform error message.
    Platform(String),
}

/// Plugin trait the platform implements to service [`ServiceContext`] helper
/// calls. Native impl (Slice D) lives in the Kotlin runtime; mock impls
/// (tests, desktop) can be plain Rust.
#[plugin(name = "istmo.service_control", crate = "::istmo_core")]
pub trait ServiceControl {
    /// Promote the service `service_id` to the foreground with `spec` as the
    /// initial notification.
    async fn start_foreground(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    /// Update the notification for `service_id`. The service must already be
    /// running in the foreground.
    async fn update_notification(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    /// Demote `service_id` back to a background service. When
    /// `remove_notification` is `true` the notification is cancelled.
    async fn stop_foreground(
        &self,
        service_id: String,
        remove_notification: bool,
    ) -> Result<(), ServiceControlError>;

    /// Acquire a wakelock tagged `tag` for `service_id`. Returns an opaque
    /// token used to release the wakelock later.
    async fn acquire_wakelock(
        &self,
        service_id: String,
        tag: String,
    ) -> Result<WakelockToken, ServiceControlError>;

    /// Release a previously acquired wakelock. Backends must treat an
    /// already-released token as a no-op.
    async fn release_wakelock(
        &self,
        service_id: String,
        token: WakelockToken,
    ) -> Result<(), ServiceControlError>;

    /// Ask the platform to stop `service_id`. The platform is expected to
    /// eventually issue an on-stop signal back into the service adapter.
    async fn stop_self(&self, service_id: String) -> Result<(), ServiceControlError>;
}

/// Half of the stop-signal channel handed to the service adapter. Signalling
/// it wakes the [`ServiceContext::stopped`] future the service impl awaits.
#[derive(Debug, Clone)]
pub struct StopNotifier {
    tx: Sender<()>,
}

impl StopNotifier {
    /// Notify the running service that the platform wants it to stop.
    /// Idempotent: repeated calls are silently ignored.
    pub fn signal(&self) {
        let _ = self.tx.try_send(());
    }
}

/// Create the stop-signal channel pair. The service adapter keeps
/// [`StopNotifier`]; the [`Receiver`] is embedded in the [`ServiceContext`].
#[must_use]
pub fn stop_channel() -> (StopNotifier, Receiver<()>) {
    let (tx, rx) = bounded(1);
    (StopNotifier { tx }, rx)
}

/// Context handed to a service impl on `on_start`. Non-`Clone`: each service
/// invocation gets exactly one context, whose lifetime bounds the service's
/// wakelocks and foreground-notification updates.
#[derive(Debug)]
pub struct ServiceContext {
    runtime: Arc<Runtime>,
    service_id: String,
    stop_rx: Receiver<()>,
    stopped_flag: AtomicBool,
}

impl ServiceContext {
    /// Construct a context. Intended for the codegen layer and tests; user
    /// code receives ready-made contexts via `on_start`.
    #[must_use]
    pub const fn new(runtime: Arc<Runtime>, service_id: String, stop_rx: Receiver<()>) -> Self {
        Self {
            runtime,
            service_id,
            stop_rx,
            stopped_flag: AtomicBool::new(false),
        }
    }

    /// The service identifier this context was created for.
    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    /// Access the underlying runtime. Useful when the service needs to
    /// acquire other istmo plugins from within its `on_start` body.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    /// Promote the service to the foreground with `spec` as the initial
    /// notification.
    pub async fn set_foreground(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "start_foreground",
            &(self.service_id.clone(), spec),
        )
        .await
    }

    /// Update the notification while the service runs in the foreground.
    pub async fn update_notification(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "update_notification",
            &(self.service_id.clone(), spec),
        )
        .await
    }

    /// Demote the service back to a background service.
    pub async fn stop_foreground(&self, remove_notification: bool) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "stop_foreground",
            &(self.service_id.clone(), remove_notification),
        )
        .await
    }

    /// Acquire a wakelock. The returned [`WakeLock`] releases the wakelock on
    /// drop; callers who care about propagating release errors should call
    /// [`WakeLock::release`] explicitly.
    pub async fn acquire_wakelock(&self, tag: &str) -> Result<WakeLock, IstmoError> {
        let payload = codec::encode(&(self.service_id.clone(), tag.to_owned()))?;
        let handle =
            self.runtime
                .call(SERVICE_CONTROL_PLUGIN_ID, None, "acquire_wakelock", payload)?;
        let bytes = decode_ok(handle.await?)?;
        let (token, _) = codec::decode::<WakelockToken>(&bytes)?;
        Ok(WakeLock {
            token,
            runtime: Arc::downgrade(&self.runtime),
            service_id: self.service_id.clone(),
        })
    }

    /// Ask the platform to stop this service. The platform is expected to
    /// send a stop signal back into this context once teardown starts.
    pub async fn stop_self(&self) -> Result<(), IstmoError> {
        call_unit(&self.runtime, "stop_self", &(self.service_id.clone(),)).await
    }

    /// Await the platform's stop signal. Resolves once the adapter observes
    /// an on-stop call from the platform, or when the notifier is dropped
    /// (which also indicates teardown). Subsequent calls resolve immediately.
    pub async fn stopped(&self) {
        if self.stopped_flag.load(Ordering::Acquire) {
            return;
        }
        let _ = self.stop_rx.recv_async().await;
        self.stopped_flag.store(true, Ordering::Release);
    }

    /// Non-blocking probe: `true` once the platform has signalled stop.
    #[must_use]
    pub fn is_stopped(&self) -> bool {
        if self.stopped_flag.load(Ordering::Acquire) {
            return true;
        }
        if self.stop_rx.is_disconnected() || !self.stop_rx.is_empty() {
            self.stopped_flag.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

/// RAII wakelock. Dropping the value releases the wakelock on a detached
/// worker thread; callers who need to observe release errors should call
/// [`Self::release`] instead.
#[derive(Debug)]
pub struct WakeLock {
    token: WakelockToken,
    runtime: Weak<Runtime>,
    service_id: String,
}

impl WakeLock {
    /// The token identifying this wakelock on the platform side.
    #[must_use]
    pub const fn token(&self) -> WakelockToken {
        self.token
    }

    /// Release the wakelock and surface any error the platform reports.
    pub async fn release(mut self) -> Result<(), IstmoError> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or(IstmoError::RuntimeNotStarted)?;
        // Prevent Drop from firing a redundant release.
        self.runtime = Weak::new();
        let service_id = mem::take(&mut self.service_id);
        release_wakelock_call(&runtime, service_id, self.token).await
    }
}

impl Drop for WakeLock {
    fn drop(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let service_id = mem::take(&mut self.service_id);
        let token = self.token;
        std::thread::spawn(move || {
            let payload = match codec::encode(&(service_id, token)) {
                Ok(p) => p,
                Err(err) => {
                    tracing::warn!(?err, "wakelock release: encode failed");
                    return;
                }
            };
            let handle =
                match runtime.call(SERVICE_CONTROL_PLUGIN_ID, None, "release_wakelock", payload) {
                    Ok(h) => h,
                    Err(err) => {
                        tracing::warn!(?err, "wakelock release: submit failed");
                        return;
                    }
                };
            match handle.recv_blocking() {
                Ok(Ok(_)) => {}
                Ok(Err(bytes)) => {
                    tracing::warn!(bytes = bytes.len(), "wakelock release: domain error");
                }
                Err(err) => {
                    tracing::warn!(?err, "wakelock release: transport error");
                }
            }
        });
    }
}

async fn call_unit<T>(
    runtime: &Arc<Runtime>,
    method: &'static str,
    args: &T,
) -> Result<(), IstmoError>
where
    T: bincode::Encode + Sync,
{
    let payload = codec::encode(args)?;
    let handle = runtime.call(SERVICE_CONTROL_PLUGIN_ID, None, method, payload)?;
    let bytes = decode_ok(handle.await?)?;
    let ((), _) = codec::decode::<()>(&bytes)?;
    Ok(())
}

async fn release_wakelock_call(
    runtime: &Arc<Runtime>,
    service_id: String,
    token: WakelockToken,
) -> Result<(), IstmoError> {
    call_unit(runtime, "release_wakelock", &(service_id, token)).await
}

fn decode_ok(result: istmo_core::CallResult) -> Result<Vec<u8>, IstmoError> {
    match result {
        Ok(bytes) => Ok(bytes),
        Err(bytes) => Err(IstmoError::PluginError { bytes }),
    }
}
