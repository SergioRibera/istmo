//! Rust-side host wiring that turns a [`PenPublisher`] into a plugin
//! dispatcher: `PenBackend` implements the [`Pen`] trait for one
//! registered window, and [`PenDesktopHost`] fans
//! [`Frame::CreateInstance`](istmo_core::Frame) frames out into fresh
//! per-window backends.
//!
//! Wiring pattern for a desktop app:
//!
//! ```ignore
//! use istmo_pen::{PenClient, PenConfig, backend::PenDesktopHost, publisher::PenPublisher};
//! use istmo::runtime;
//!
//! let publisher = PenPublisher::install(&runtime);
//! publisher.register_window(1, &window)?;
//!
//! let rt = runtime! {
//!     plugins: [PenClient],
//!     hosts: [PenDesktopHost::new(publisher.clone())],
//! };
//! let client = PenClient::from_runtime_with(&rt, PenConfig::new(1)).await?;
//! let events = client.events()?;      // Rust-hosted stream, no wire crossing
//! ```
//!
//! Only compiled on desktop targets (`windows`, `linux`, `macos`);
//! mobile plugins are hosted by their language-side backends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use flume::Receiver;
use istmo_core::{
    CancelToken, Dispatch, DispatchError, DispatchFuture, InstanceId, Outcome, Plugin, codec,
};

use crate::publisher::PenPublisher;
use crate::{
    PEN_PLUGIN_ID, Pen, PenCapabilities, PenConfig, PenError, PenEvent, PenHoverEvent,
};

/// Per-instance [`Pen`] implementation. Holds the [`Receiver`] halves
/// handed out by [`PenPublisher::subscribe_events`] /
/// [`PenPublisher::subscribe_hover`] plus a static capability
/// descriptor for the compile target.
#[derive(Debug)]
pub struct PenBackend {
    window_id: u64,
    events_rx: Mutex<Option<Receiver<PenEvent>>>,
    hover_rx: Mutex<Option<Receiver<PenHoverEvent>>>,
}

impl PenBackend {
    /// Attach a backend to the flume receivers a publisher hands out
    /// for `window_id`. `None` receivers mean the window was never
    /// registered on the publisher or its receivers were already
    /// consumed — the client will still see empty streams.
    #[must_use]
    pub fn new(
        window_id: u64,
        events_rx: Option<Receiver<PenEvent>>,
        hover_rx: Option<Receiver<PenHoverEvent>>,
    ) -> Self {
        Self {
            window_id,
            events_rx: Mutex::new(events_rx),
            hover_rx: Mutex::new(hover_rx),
        }
    }

    /// Window id this backend was constructed for.
    #[must_use]
    pub const fn window_id(&self) -> u64 {
        self.window_id
    }
}

impl Pen for PenBackend {
    fn events(&self) -> Receiver<PenEvent> {
        take_or_closed(&self.events_rx)
    }

    fn hover(&self) -> Receiver<PenHoverEvent> {
        take_or_closed(&self.hover_rx)
    }

    async fn capabilities(&self) -> Result<PenCapabilities, PenError> {
        Ok(platform_capabilities())
    }

    async fn set_prediction_enabled(
        &self,
        _cancel: CancelToken,
        _enabled: bool,
    ) -> Result<(), PenError> {
        // Windows / macOS / Linux do not currently expose a native
        // sample predictor — the `predicted` slot on `PenEvent::Move`
        // stays empty regardless of this setting.
        Ok(())
    }
}

fn take_or_closed<T>(slot: &Mutex<Option<Receiver<T>>>) -> Receiver<T> {
    let taken = {
        let mut guard = match slot.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.take()
    };
    taken.unwrap_or_else(|| {
        let (_tx, rx) = flume::bounded(0);
        rx
    })
}

const fn platform_capabilities() -> PenCapabilities {
    PenCapabilities {
        pressure: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        tilt: cfg!(any(target_os = "windows", target_os = "macos")),
        azimuth: cfg!(target_os = "macos"),
        altitude: false,
        twist: cfg!(any(target_os = "windows", target_os = "macos")),
        tangential_pressure: cfg!(target_os = "macos"),
        hover: cfg!(target_os = "windows"),
        predicted: false,
        coalesced: cfg!(target_os = "windows"),
        barrel_button: cfg!(any(target_os = "windows", target_os = "macos")),
        eraser: cfg!(target_os = "windows"),
    }
}

/// Hand-written [`Dispatch`] implementation for the [`Pen`] plugin.
///
/// Rust-hosted stateful plugins are not modelled by the current
/// [`#[plugin]`](istmo_macros::plugin) macro (which emits a
/// single-instance `PenHost<Impl>`); this type routes
/// [`Frame::CreateInstance`](istmo_core::Frame) into a per-window
/// [`PenBackend`] and multiplexes subsequent method calls by
/// `instance_id`.
#[derive(Debug)]
pub struct PenDesktopHost {
    publisher: Arc<PenPublisher>,
    instances: Mutex<HashMap<InstanceId, Arc<PenBackend>>>,
    next_instance: AtomicU64,
}

impl PenDesktopHost {
    /// Build a host bound to `publisher`. Register it on the runtime
    /// via [`Runtime::register_host`](istmo_core::Runtime::register_host)
    /// or through the `hosts:` section of `istmo::runtime!`.
    #[must_use]
    pub fn new(publisher: Arc<PenPublisher>) -> Self {
        Self {
            publisher,
            instances: Mutex::new(HashMap::new()),
            next_instance: AtomicU64::new(1),
        }
    }

    fn allocate_instance_id(&self) -> InstanceId {
        InstanceId::new(self.next_instance.fetch_add(1, Ordering::Relaxed))
    }

    fn store_backend(&self, id: InstanceId, backend: Arc<PenBackend>) {
        let mut guard = match self.instances.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.insert(id, backend);
    }

    fn lookup(&self, id: InstanceId) -> Option<Arc<PenBackend>> {
        let guard = match self.instances.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.get(&id).cloned()
    }

    fn remove(&self, id: InstanceId) -> Option<Arc<PenBackend>> {
        let mut guard = match self.instances.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        guard.remove(&id)
    }
}

impl Plugin for PenDesktopHost {
    const PLUGIN_ID: &'static str = PEN_PLUGIN_ID;
}

impl Dispatch for PenDesktopHost {
    fn plugin_id(&self) -> &'static str {
        PEN_PLUGIN_ID
    }

    fn create_instance<'a>(
        &'a self,
        config_payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            let (config, _) = codec::decode::<PenConfig>(config_payload)
                .map_err(DispatchError::Decode)?;
            let events_rx = self.publisher.subscribe_events(config.window_id);
            let hover_rx = self.publisher.subscribe_hover(config.window_id);
            let backend = Arc::new(PenBackend::new(config.window_id, events_rx, hover_rx));
            let instance_id = self.allocate_instance_id();
            self.store_backend(instance_id, backend);
            let bytes = codec::encode(&instance_id).map_err(DispatchError::Encode)?;
            Ok(Outcome::Ok(bytes))
        })
    }

    fn destroy_instance(&self, instance_id: InstanceId) {
        self.remove(instance_id);
    }

    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        Box::pin(async move {
            let instance_id = instance_id.ok_or_else(|| {
                DispatchError::UnknownMethod(format!("{method} requires an instance id"))
            })?;
            let backend = self
                .lookup(instance_id)
                .ok_or_else(|| DispatchError::UnknownMethod(format!("unknown instance {instance_id:?}")))?;
            match method {
                "events" => Ok(open_stream::<PenEvent>(backend.events())),
                "hover" => Ok(open_stream::<PenHoverEvent>(backend.hover())),
                "capabilities" => {
                    let result = backend.capabilities().await;
                    encode_domain_result(result)
                }
                "set_prediction_enabled" => {
                    let (args, _) = codec::decode::<(bool,)>(payload)
                        .map_err(DispatchError::Decode)?;
                    let (enabled,) = args;
                    let result = backend.set_prediction_enabled(cancel, enabled).await;
                    encode_domain_result(result)
                }
                other => Err(DispatchError::UnknownMethod(other.to_owned())),
            }
        })
    }
}

fn open_stream<T>(rx: Receiver<T>) -> Outcome
where
    T: bincode::Encode + Send + 'static,
{
    let (etx, erx) = flume::unbounded::<Vec<u8>>();
    std::thread::spawn(move || {
        while let Ok(item) = rx.recv() {
            let Ok(bytes) = codec::encode(&item) else {
                break;
            };
            if etx.send(bytes).is_err() {
                break;
            }
        }
    });
    Outcome::StreamOpened(erx)
}

fn encode_domain_result<T>(result: Result<T, PenError>) -> Result<Outcome, DispatchError>
where
    T: bincode::Encode,
{
    match result {
        Ok(value) => codec::encode(&value)
            .map(Outcome::Ok)
            .map_err(DispatchError::Encode),
        Err(err) => codec::encode(&err)
            .map(Outcome::DomainError)
            .map_err(DispatchError::Encode),
    }
}

