//! Static device metadata: model, manufacturer, OS, arch, form factor,
//! CPU, RAM, screen, optional vendor id.
//!
//! One `LatestValueSlot` carries a [`DeviceInfo`] snapshot the native
//! side publishes once at startup. Rust-known fields (`arch`,
//! `os_name`, `cpu_cores`) always come straight from the standard
//! library; everything else is best-effort and may be absent in the
//! Rust-side fallback until the platform publishes.
//!
//! Desktop targets (Linux / macOS / Windows) expose
//! [`DeviceInfoPublisher`] which emits the Rust-known snapshot + any
//! platform values it can resolve without a system dep.

use istmo_core::IstmoError;
use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod publisher;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use publisher::DeviceInfoPublisher;

/// Plugin id of the device info snapshot.
pub const DEVICE_INFO_PLUGIN_ID: &str = "istmo.device_info";
/// Early-event channel the device snapshot is published on.
pub const DEVICE_INFO_CHANNEL: &str = "istmo.device_info";

/// Static description of the device.
#[message(wincode = "::wincode")]
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

#[message(wincode = "::wincode")]
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

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenInfo {
    pub logical_width: u32,
    pub logical_height: u32,
    /// `density_dpi / 160` on Android, `UIScreen.scale` on iOS/macOS,
    /// `scale_factor` on winit-based desktops.
    pub scale: f32,
    pub refresh_hz: Option<f32>,
}

impl LatestValue for DeviceInfo {
    const PLUGIN_ID: &'static str = DEVICE_INFO_PLUGIN_ID;
    const CHANNEL: &'static str = DEVICE_INFO_CHANNEL;
}

/// Reader for the device snapshot the platform publishes.
pub type DeviceInfoClient = LatestClient<DeviceInfo>;

/// Device snapshots as they are published.
pub type DeviceInfoStream = EarlyStream<DeviceInfo>;

impl LatestClient<DeviceInfo> {
    /// [`current`](Self::current) if present, otherwise the Rust-side
    /// [`rust_fallback`] snapshot. Useful on desktops where no platform
    /// publisher is wired yet.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn current_or_fallback(&self) -> Result<DeviceInfo, IstmoError> {
        Ok(self.current()?.unwrap_or_else(rust_fallback))
    }
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
