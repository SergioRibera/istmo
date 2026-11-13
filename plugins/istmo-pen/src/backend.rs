//! Desktop-side factory that pairs the macro-emitted
//! [`PenHost`](crate::PenHost) with a [`PenPublisher`].
//!
//! Each `CreateInstance` frame the runtime routes
//! to this host produces a fresh [`PenBackend`]; every stream it opens
//! is a new subscription to the matching window on the publisher.
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

use flume::Receiver;
use istmo_core::CancelToken;

use crate::publisher::PenPublisher;
use crate::{Pen, PenCapabilities, PenConfig, PenError, PenEvent, PenFactory, PenHoverEvent};

/// Per-instance [`Pen`] implementation bound to one window of a
/// [`PenPublisher`]. Each [`Pen::events`] / [`Pen::hover`] call
/// subscribes anew, so several streams on one instance each see every
/// sample.
#[derive(Debug)]
pub struct PenBackend {
    window_id: u64,
    publisher: Arc<PenPublisher>,
}

impl PenBackend {
    /// Backend for `window_id` on `publisher`. An id the publisher does
    /// not know yields streams that are already closed.
    #[must_use]
    pub const fn new(window_id: u64, publisher: Arc<PenPublisher>) -> Self {
        Self {
            window_id,
            publisher,
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
        self.publisher
            .subscribe_events(self.window_id)
            .unwrap_or_else(closed)
    }

    fn hover(&self) -> Receiver<PenHoverEvent> {
        self.publisher
            .subscribe_hover(self.window_id)
            .unwrap_or_else(closed)
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

fn closed<T>() -> Receiver<T> {
    let (_tx, rx) = flume::bounded(0);
    rx
}

const fn platform_capabilities() -> PenCapabilities {
    // Static maximums the backend can decode. Some devices expose
    // fewer physical buttons; consumers wanting the *actual* count for
    // a specific pen must snapshot state at proximity-in (future
    // extension). Wacom pens on Linux commonly ship 2 barrel buttons +
    // an eraser tail — libinput surfaces up to `BTN_STYLUS3`.
    let button_count: u32 = if cfg!(target_os = "linux") {
        3
    } else if cfg!(any(target_os = "windows", target_os = "macos")) {
        2
    } else {
        0
    };
    PenCapabilities {
        pressure: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        tilt: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        azimuth: false,
        altitude: false,
        twist: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        tangential_pressure: cfg!(target_os = "macos"),
        hover: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
        predicted: false,
        coalesced: cfg!(target_os = "windows"),
        button_count,
        eraser: cfg!(any(
            target_os = "windows",
            target_os = "macos",
            target_os = "linux"
        )),
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
    pub const fn new(publisher: Arc<PenPublisher>) -> Self {
        Self { publisher }
    }

    #[must_use]
    pub const fn publisher(&self) -> &Arc<PenPublisher> {
        &self.publisher
    }
}

impl PenFactory for PenPublisherFactory {
    type Instance = PenBackend;

    fn create(&self, config: PenConfig) -> PenBackend {
        PenBackend::new(config.window_id, Arc::clone(&self.publisher))
    }
}
