//! Available storage on the primary (and optionally external) volume.
//!
//! Published as a latest-value stream — platforms re-emit on
//! low-storage warnings (`UIApplicationDidReceiveMemoryWarningNotification`
//! does not help here; use `NSFileManager.attributesOfFileSystemForPath`
//! on wake; Android's `android.intent.action.DEVICE_STORAGE_LOW` /
//! `DEVICE_STORAGE_OK`). No permissions required when reading only the
//! app's scoped storage volume.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const STORAGE_INFO_PLUGIN_ID: &str = "istmo.storage_info";
pub const STORAGE_INFO_CHANNEL: &str = "istmo.storage_info";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageInfo {
    pub primary_free_bytes: u64,
    pub primary_total_bytes: u64,
    pub external: Option<ExternalStorage>,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalStorage {
    pub free_bytes: u64,
    pub total_bytes: u64,
    /// Underlying medium — a removable SD card on Android, an attached
    /// disk on desktop, `Unknown` when the platform does not say.
    pub medium: ExternalMedium,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExternalMedium {
    #[default]
    Unknown,
    SdCard,
    Usb,
    Network,
}

#[derive(Debug, Clone)]
pub struct StorageInfoClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for StorageInfoClient {
    const PLUGIN_ID: &'static str = STORAGE_INFO_PLUGIN_ID;
}

impl StorageInfoClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(STORAGE_INFO_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<StorageInfo>, IstmoError> {
        self.slot
            .peek()
            .map_or(Ok(None), |b| decode(&b).map(Some))
    }

    #[must_use]
    pub fn updates(&self) -> StorageInfoStream {
        StorageInfoStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct StorageInfoStream {
    rx: Receiver<Vec<u8>>,
}

impl StorageInfoStream {
    pub fn recv(&self) -> Result<StorageInfo, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<StorageInfo, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<StorageInfo, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<StorageInfo, IstmoError> {
    let (v, _) = codec::decode::<StorageInfo>(bytes)?;
    Ok(v)
}
