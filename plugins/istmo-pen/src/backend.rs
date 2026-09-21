//! Desktop-side factory that pairs the macro-emitted [`PenHost`] with
//! a [`PenPublisher`]. Each `CreateInstance` frame the runtime routes
//! to this host produces a fresh [`PenBackend`] whose flume receivers
//! are already subscribed to the matching per-window channel on the
//! publisher.
//!
//! Wiring pattern for a desktop app:
//!
//! ```ignore
//! use istmo::runtime;
//! use istmo_pen::{
//!     PenClient, PenConfig, PenHost, backend::PenPublisherFactory,
//!     publisher::PenPublisher,
//! };
//!
//! let publisher = PenPublisher::install(&runtime);
//! publisher.register_window(1, &window)?;
//!
//! let rt = runtime! {
//!     plugins: [PenClient],
//!     hosts: [PenHost::new(PenPublisherFactory::new(publisher))],
//! };
//! let client = PenClient::from_runtime_with(&rt, PenConfig::new(1)).await?;
//! let events = client.events()?;
//! ```
//!
//! Only compiled on desktop targets (`windows`, `linux`, `macos`);
//! mobile plugins are hosted by their language-side backends.

use std::sync::Arc;
use std::sync::Mutex;

use flume::Receiver;
use istmo_core::CancelToken;

use crate::publisher::PenPublisher;
use crate::{
    Pen, PenCapabilities, PenConfig, PenError, PenEvent, PenFactory, PenHoverEvent,
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
        azimuth: false,
        altitude: false,
        twist: cfg!(any(target_os = "windows", target_os = "macos")),
        tangential_pressure: cfg!(target_os = "macos"),
        hover: cfg!(any(target_os = "windows", target_os = "macos")),
        predicted: false,
        coalesced: cfg!(target_os = "windows"),
        barrel_button: cfg!(any(target_os = "windows", target_os = "macos")),
        eraser: cfg!(any(target_os = "windows", target_os = "macos")),
    }
}

/// Factory that constructs a [`PenBackend`] bound to a shared
/// [`PenPublisher`]. Register with the macro-emitted
/// [`PenHost`](crate::PenHost) via `PenHost::new(PenPublisherFactory::new(publisher))`.
#[derive(Debug)]
pub struct PenPublisherFactory {
    publisher: Arc<PenPublisher>,
}

impl PenPublisherFactory {
    #[must_use]
    pub fn new(publisher: Arc<PenPublisher>) -> Self {
        Self { publisher }
    }

    #[must_use]
    pub fn publisher(&self) -> &Arc<PenPublisher> {
        &self.publisher
    }
}

impl PenFactory for PenPublisherFactory {
    type Instance = PenBackend;

    fn create(&self, config: PenConfig) -> PenBackend {
        let events_rx = self.publisher.subscribe_events(config.window_id);
        let hover_rx = self.publisher.subscribe_hover(config.window_id);
        PenBackend::new(config.window_id, events_rx, hover_rx)
    }
}
