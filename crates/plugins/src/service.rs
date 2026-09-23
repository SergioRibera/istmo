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

pub const SERVICE_CONTROL_PLUGIN_ID: &str = "istmo.service_control";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationSpec {
    pub channel_id: String,

    pub notification_id: i32,

    pub title: String,

    pub body: String,

    pub small_icon: Option<String>,

    pub ongoing: bool,

    pub foreground_service_type: Option<u32>,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct WakelockToken(pub u64);

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceControlError {
    UnknownService(String),

    WakelockDenied(String),

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

#[plugin(name = "istmo.service_control", crate = "::istmo_core")]
pub trait ServiceControl {
    async fn start_foreground(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    async fn update_notification(
        &self,
        service_id: String,
        spec: NotificationSpec,
    ) -> Result<(), ServiceControlError>;

    async fn stop_foreground(
        &self,
        service_id: String,
        remove_notification: bool,
    ) -> Result<(), ServiceControlError>;

    async fn acquire_wakelock(
        &self,
        service_id: String,
        tag: String,
    ) -> Result<WakelockToken, ServiceControlError>;

    async fn release_wakelock(
        &self,
        service_id: String,
        token: WakelockToken,
    ) -> Result<(), ServiceControlError>;

    async fn stop_self(&self, service_id: String) -> Result<(), ServiceControlError>;
}

#[derive(Debug, Clone)]
pub struct StopNotifier {
    tx: Sender<()>,
}

impl StopNotifier {
    pub fn signal(&self) {
        let _ = self.tx.try_send(());
    }
}

#[must_use]
pub fn stop_channel() -> (StopNotifier, Receiver<()>) {
    let (tx, rx) = bounded(1);
    (StopNotifier { tx }, rx)
}

#[derive(Debug)]
pub struct ServiceContext {
    runtime: Arc<Runtime>,
    service_id: String,
    stop_rx: Receiver<()>,
    stopped_flag: AtomicBool,
    cancel: CancelToken,
}

impl ServiceContext {
    #[must_use]
    pub const fn new(
        runtime: Arc<Runtime>,
        service_id: String,
        stop_rx: Receiver<()>,
        cancel: CancelToken,
    ) -> Self {
        Self {
            runtime,
            service_id,
            stop_rx,
            stopped_flag: AtomicBool::new(false),
            cancel,
        }
    }

    #[must_use]
    pub const fn cancel_token(&self) -> &CancelToken {
        &self.cancel
    }

    #[must_use]
    pub fn service_id(&self) -> &str {
        &self.service_id
    }

    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.runtime
    }

    pub async fn set_foreground(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "start_foreground",
            &(self.service_id.clone(), spec),
        )
        .await
    }

    pub async fn update_notification(&self, spec: NotificationSpec) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "update_notification",
            &(self.service_id.clone(), spec),
        )
        .await
    }

    pub async fn stop_foreground(&self, remove_notification: bool) -> Result<(), IstmoError> {
        call_unit(
            &self.runtime,
            "stop_foreground",
            &(self.service_id.clone(), remove_notification),
        )
        .await
    }

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

    pub async fn stop_self(&self) -> Result<(), IstmoError> {
        call_unit(&self.runtime, "stop_self", &(self.service_id.clone(),)).await
    }

    pub async fn stopped(&self) {
        if self.is_stopped() {
            return;
        }
        let _ = self.stop_rx.recv_async().await;
        self.stopped_flag.store(true, Ordering::Release);
    }

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

#[derive(Debug)]
pub struct WakeLock {
    token: WakelockToken,
    runtime: Weak<Runtime>,
    service_id: String,
}

impl WakeLock {
    #[must_use]
    pub const fn token(&self) -> WakelockToken {
        self.token
    }

    pub async fn release(mut self) -> Result<(), IstmoError> {
        let runtime = self
            .runtime
            .upgrade()
            .ok_or(IstmoError::RuntimeNotStarted)?;

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
        let payload = match codec::encode(&(service_id, self.token)) {
            Ok(p) => p,
            Err(err) => {
                tracing::warn!(?err, "wakelock release: encode failed");
                return;
            }
        };
        if let Err(err) =
            runtime.notify(SERVICE_CONTROL_PLUGIN_ID, None, "release_wakelock", payload)
        {
            tracing::warn!(?err, "wakelock release: notify submit failed");
        }
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
