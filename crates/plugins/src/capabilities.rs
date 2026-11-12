//! Static device capability flags: biometric, NFC, haptics, camera,
//! GPS, bluetooth.
//!
//! Published once at startup. Values are detected from
//! `PackageManager.hasSystemFeature` on Android, `LAContext` /
//! `NFCNDEFReaderSession` / sensor availability APIs on Apple, and
//! equivalent probes on desktop. The Rust fallback marks every flag as
//! `false` until the platform reports.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the capability flags.
pub const CAPABILITIES_PLUGIN_ID: &str = "istmo.capabilities";
/// Early-event channel the capability flags are published on.
pub const CAPABILITIES_CHANNEL: &str = "istmo.capabilities";

/// Hardware features the device offers.
#[message(wincode = "::wincode")]
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

impl LatestValue for Capabilities {
    const PLUGIN_ID: &'static str = CAPABILITIES_PLUGIN_ID;
    const CHANNEL: &'static str = CAPABILITIES_CHANNEL;
}

/// Reader for the capability flags the platform publishes.
///
/// [`LatestClient::current_or_default`] yields the all-`false` default
/// on desktops where no capability publisher is wired.
pub type CapabilitiesClient = LatestClient<Capabilities>;

/// Capability flags as they are published.
pub type CapabilitiesStream = EarlyStream<Capabilities>;
