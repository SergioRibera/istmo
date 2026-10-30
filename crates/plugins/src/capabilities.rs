//! Static device capability flags: biometric, NFC, haptics, camera,
//! GPS, bluetooth.
//!
//! Published once at startup. Values are detected from
//! `PackageManager.hasSystemFeature` on Android, `LAContext` /
//! `NFCNDEFReaderSession` / sensor availability APIs on Apple, and
//! equivalent probes on desktop. The Rust fallback marks every flag as
//! `false` until the platform reports.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const CAPABILITIES_PLUGIN_ID: &str = "istmo.capabilities";
pub const CAPABILITIES_CHANNEL: &str = "istmo.capabilities";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[allow(clippy::struct_excessive_bools)]
pub struct Capabilities {
    pub has_biometric: bool,
    pub has_face_id: bool,
    pub has_fingerprint: bool,
    pub has_nfc: bool,
    pub has_haptics: bool,
    pub has_camera_front: bool,
    pub has_camera_back: bool,
    pub has_gps: bool,
    pub has_bluetooth: bool,
    pub has_microphone: bool,
    pub has_telephony: bool,
}

#[derive(Debug, Clone)]
pub struct CapabilitiesClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for CapabilitiesClient {
    const PLUGIN_ID: &'static str = CAPABILITIES_PLUGIN_ID;
}

impl CapabilitiesClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(CAPABILITIES_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<Capabilities>, IstmoError> {
        self.slot
            .peek()
            .map_or(Ok(None), |b| decode(&b).map(Some))
    }

    /// Returns [`current`](Self::current) if present, otherwise the
    /// all-`false` default. Useful on desktops where no capability
    /// publisher is wired.
    pub fn current_or_default(&self) -> Result<Capabilities, IstmoError> {
        Ok(self.current()?.unwrap_or_default())
    }

    #[must_use]
    pub fn updates(&self) -> CapabilitiesStream {
        CapabilitiesStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct CapabilitiesStream {
    rx: Receiver<Vec<u8>>,
}

impl CapabilitiesStream {
    pub fn recv(&self) -> Result<Capabilities, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<Capabilities, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<Capabilities, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<Capabilities, IstmoError> {
    let (v, _) = codec::decode::<Capabilities>(bytes)?;
    Ok(v)
}
