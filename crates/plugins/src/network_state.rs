//! Reachability + link classification stream.
//!
//! Published by native sides on connectivity change notifications:
//! - Android: `ConnectivityManager.NetworkCallback`
//!   (requires `android.permission.ACCESS_NETWORK_STATE` in the
//!   consuming app's manifest).
//! - Apple: `NWPathMonitor` on a serial queue.
//! - Linux: `NetworkManager` D-Bus `org.freedesktop.NetworkManager`
//!   `StateChanged` signal.
//! - Windows: `INetworkListManager::NetworkConnectivityChanged`.
//! - Web: `online` / `offline` window events + `navigator.connection`.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the network state stream.
pub const NETWORK_STATE_PLUGIN_ID: &str = "istmo.network_state";
/// Early-event channel the network snapshot is published on.
pub const NETWORK_STATE_CHANNEL: &str = "istmo.network_state";

/// Connectivity snapshot.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "independent wire flags mirroring the platform APIs"
)]
pub struct NetworkState {
    /// A route to the internet exists.
    pub online: bool,
    /// Kind of the link carrying the default route.
    pub link: LinkKind,
    /// User is on a metered connection (mobile data, hotspot). Treat
    /// as hint to avoid prefetches / background sync.
    pub metered: bool,
    /// iOS `NWPath.isExpensive` — cellular or tethered.
    pub expensive: bool,
    /// iOS Low Data Mode / Android Data Saver.
    pub constrained: bool,
}

/// Physical or virtual kind of a network link.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LinkKind {
    /// No usable link.
    #[default]
    None,
    /// Wi-Fi.
    Wifi,
    /// Mobile data.
    Cellular,
    /// Wired Ethernet.
    Ethernet,
    /// VPN or tunnel.
    Vpn,
    /// Bluetooth tethering.
    Bluetooth,
    /// Anything else.
    Other,
}

impl LatestValue for NetworkState {
    const PLUGIN_ID: &'static str = NETWORK_STATE_PLUGIN_ID;
    const CHANNEL: &'static str = NETWORK_STATE_CHANNEL;
}

/// Reader for the connectivity snapshot the platform publishes.
pub type NetworkStateClient = LatestClient<NetworkState>;

/// Connectivity snapshots as they are published.
pub type NetworkStateStream = EarlyStream<NetworkState>;
