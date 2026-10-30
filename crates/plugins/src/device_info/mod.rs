//! Static device metadata: model, manufacturer, OS, arch, form factor,
//! CPU, RAM, screen, optional vendor id.
//!
//! One `LatestValueSlot` carries a [`DeviceInfo`] snapshot the native
//! side publishes once at startup. Rust-known fields (`arch`,
//! `os_name`, `cpu_cores`) always come straight from the standard
//! library; everything else is best-effort and may be absent in the
//! Rust-side fallback until the platform publishes.
//!
//! The `desktop-publisher` feature on this crate exposes
//! [`DeviceInfoPublisher`] which emits the Rust-known snapshot + any
//! Linux / macOS values it can resolve without a system dep.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

#[cfg(feature = "desktop-publisher")]
pub mod publisher;

#[cfg(feature = "desktop-publisher")]
pub use publisher::DeviceInfoPublisher;

pub const DEVICE_INFO_PLUGIN_ID: &str = "istmo.device_info";
pub const DEVICE_INFO_CHANNEL: &str = "istmo.device_info";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceInfo {
    /// Marketing model identifier when the platform exposes it
    /// (`Pixel 8`, `iPhone15,2`). `None` when no reliable source
    /// exists (most Linux desktops).
    pub model: Option<String>,

    pub manufacturer: Option<String>,

    /// `std::env::consts::OS` — always populated.
    pub os_name: String,

    /// Human-readable OS version (`14`, `17.2`, `11 23H2`,
    /// `6.9.3-arch1`). `None` when unresolved.
    pub os_version: Option<String>,

    /// `std::env::consts::ARCH` — always populated.
    pub arch: String,

    pub form_factor: FormFactor,

    /// `std::thread::available_parallelism`. Falls back to `1`.
    pub cpu_cores: u32,

    pub ram_total_bytes: Option<u64>,

    pub screen: Option<ScreenInfo>,

    /// Per-vendor stable identifier (`identifierForVendor` on Apple,
    /// `Settings.Secure.ANDROID_ID` on Android, UUID persisted via
    /// `DataStore` on desktop). Deliberately opt-in — the platform
    /// publisher is expected to omit this field unless the app wires
    /// a dedicated capability flow. `None` outside those cases.
    pub vendor_id: Option<String>,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum FormFactor {
    Phone,
    Tablet,
    Watch,
    Tv,
    Desktop,
    Web,
    #[default]
    Unknown,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenInfo {
    pub logical_width: u32,
    pub logical_height: u32,
    /// `density_dpi / 160` on Android, `UIScreen.scale` on iOS/macOS,
    /// `scale_factor` on winit-based desktops.
    pub scale: f32,
    pub refresh_hz: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct DeviceInfoClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for DeviceInfoClient {
    const PLUGIN_ID: &'static str = DEVICE_INFO_PLUGIN_ID;
}

impl DeviceInfoClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(DEVICE_INFO_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<DeviceInfo>, IstmoError> {
        self.slot
            .peek()
            .map_or(Ok(None), |b| decode(&b).map(Some))
    }

    /// Returns [`current`](Self::current) if present, otherwise the
    /// Rust-side fallback snapshot built from `std::env::consts` +
    /// `std::thread::available_parallelism`. Useful on desktops where
    /// no platform publisher is wired yet.
    pub fn current_or_fallback(&self) -> Result<DeviceInfo, IstmoError> {
        if let Some(current) = self.current()? {
            return Ok(current);
        }
        Ok(rust_fallback())
    }

    #[must_use]
    pub fn updates(&self) -> DeviceInfoStream {
        DeviceInfoStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct DeviceInfoStream {
    rx: Receiver<Vec<u8>>,
}

impl DeviceInfoStream {
    pub fn recv(&self) -> Result<DeviceInfo, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<DeviceInfo, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<DeviceInfo, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<DeviceInfo, IstmoError> {
    let (v, _) = codec::decode::<DeviceInfo>(bytes)?;
    Ok(v)
}

/// Rust-only snapshot — fields that platforms must supply (model,
/// manufacturer, specific OS version, RAM, screen) are left as `None`.
#[must_use]
pub fn rust_fallback() -> DeviceInfo {
    DeviceInfo {
        model: None,
        manufacturer: None,
        os_name: std::env::consts::OS.to_owned(),
        os_version: None,
        arch: std::env::consts::ARCH.to_owned(),
        form_factor: rust_form_factor(),
        cpu_cores: std::thread::available_parallelism()
            .map_or(1, |n| u32::try_from(n.get()).unwrap_or(u32::MAX)),
        ram_total_bytes: None,
        screen: None,
        vendor_id: None,
    }
}

fn rust_form_factor() -> FormFactor {
    match std::env::consts::OS {
        "android" | "ios" => FormFactor::Phone,
        "linux" | "macos" | "windows" | "freebsd" | "netbsd" | "openbsd" | "dragonfly" => {
            FormFactor::Desktop
        }
        _ => {
            #[cfg(target_arch = "wasm32")]
            {
                FormFactor::Web
            }
            #[cfg(not(target_arch = "wasm32"))]
            {
                FormFactor::Unknown
            }
        }
    }
}
