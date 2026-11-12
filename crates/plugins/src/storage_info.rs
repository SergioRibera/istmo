//! Available storage on the primary (and optionally external) volume.
//!
//! Published as a latest-value stream — platforms re-emit on
//! low-storage warnings (`UIApplicationDidReceiveMemoryWarningNotification`
//! does not help here; use `NSFileManager.attributesOfFileSystemForPath`
//! on wake; Android's `android.intent.action.DEVICE_STORAGE_LOW` /
//! `DEVICE_STORAGE_OK`). No permissions required when reading only the
//! app's scoped storage volume.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the storage info stream.
pub const STORAGE_INFO_PLUGIN_ID: &str = "istmo.storage_info";
/// Early-event channel the storage snapshot is published on.
pub const STORAGE_INFO_CHANNEL: &str = "istmo.storage_info";

/// Free and total space on the app's volumes.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StorageInfo {
    /// Bytes available to the app on the primary volume.
    pub primary_free_bytes: u64,
    /// Size of the primary volume in bytes.
    pub primary_total_bytes: u64,
    /// External volume, when one is mounted and reported.
    pub external: Option<ExternalStorage>,
}

/// Space on an external volume.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ExternalStorage {
    /// Bytes available to the app.
    pub free_bytes: u64,
    /// Size of the volume in bytes.
    pub total_bytes: u64,
    /// Underlying medium — a removable SD card on Android, an attached
    /// disk on desktop, `Unknown` when the platform does not say.
    pub medium: ExternalMedium,
}

/// Medium behind an external volume.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ExternalMedium {
    /// Not reported.
    #[default]
    Unknown,
    /// Removable SD card.
    SdCard,
    /// USB mass storage.
    Usb,
    /// Network share.
    Network,
}

impl LatestValue for StorageInfo {
    const PLUGIN_ID: &'static str = STORAGE_INFO_PLUGIN_ID;
    const CHANNEL: &'static str = STORAGE_INFO_CHANNEL;
}

/// Reader for the storage snapshot the platform publishes.
pub type StorageInfoClient = LatestClient<StorageInfo>;

/// Storage snapshots as they are published.
pub type StorageInfoStream = EarlyStream<StorageInfo>;
