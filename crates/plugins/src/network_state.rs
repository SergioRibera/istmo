//! Reachability + link classification stream.
//!
//! Published by native sides on connectivity change notifications:
//! - Android: `ConnectivityManager.NetworkCallback`
//!   (requires `android.permission.ACCESS_NETWORK_STATE` in the
//!   consuming app's manifest).
//! - Apple: `NWPathMonitor` on a serial queue.
//! - Linux: NetworkManager D-Bus `org.freedesktop.NetworkManager`
//!   `StateChanged` signal.
//! - Windows: `INetworkListManager::NetworkConnectivityChanged`.
//! - Web: `online` / `offline` window events + `navigator.connection`.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const NETWORK_STATE_PLUGIN_ID: &str = "istmo.network_state";
pub const NETWORK_STATE_CHANNEL: &str = "istmo.network_state";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NetworkState {
    pub online: bool,
    pub link: LinkKind,
    /// User is on a metered connection (mobile data, hotspot). Treat
    /// as hint to avoid prefetches / background sync.
    pub metered: bool,
    /// iOS `NWPath.isExpensive` — cellular or tethered.
    pub expensive: bool,
    /// iOS Low Data Mode / Android Data Saver.
    pub constrained: bool,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LinkKind {
    #[default]
    None,
    Wifi,
    Cellular,
    Ethernet,
    Vpn,
    Bluetooth,
    Other,
}

#[derive(Debug, Clone)]
pub struct NetworkStateClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for NetworkStateClient {
    const PLUGIN_ID: &'static str = NETWORK_STATE_PLUGIN_ID;
}

impl NetworkStateClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(NETWORK_STATE_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<NetworkState>, IstmoError> {
        self.slot.peek().map_or(Ok(None), |b| decode(&b).map(Some))
    }

    #[must_use]
    pub fn updates(&self) -> NetworkStateStream {
        NetworkStateStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct NetworkStateStream {
    rx: Receiver<Vec<u8>>,
}

impl NetworkStateStream {
    pub fn recv(&self) -> Result<NetworkState, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<NetworkState, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<NetworkState, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<NetworkState, IstmoError> {
    let (v, _) = codec::decode::<NetworkState>(bytes)?;
    Ok(v)
}
