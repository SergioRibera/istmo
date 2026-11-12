//! Battery level, charging and thermal state stream.
//!
//! Published by native observers:
//! - Android: `Intent.ACTION_BATTERY_CHANGED` sticky receiver +
//!   `PowerManager.isPowerSaveMode` + `getCurrentThermalStatus`.
//! - Apple: `UIDevice.batteryLevel` / `batteryState` with
//!   `batteryMonitoringEnabled = true` (iOS); `IOPowerSources` on
//!   macOS; `ProcessInfo.isLowPowerModeEnabled` +
//!   `ProcessInfo.thermalState`.
//! - Linux: `UPower` D-Bus `org.freedesktop.UPower.Device`.
//! - Windows: `GetSystemPowerStatus`.
//! - Web: `navigator.getBattery()` (deprecated, best-effort).
//!
//! No special permissions are required on any platform.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the battery state stream.
pub const BATTERY_STATE_PLUGIN_ID: &str = "istmo.battery_state";
/// Early-event channel the battery snapshot is published on.
pub const BATTERY_STATE_CHANNEL: &str = "istmo.battery_state";

impl LatestValue for BatteryState {
    const PLUGIN_ID: &'static str = BATTERY_STATE_PLUGIN_ID;
    const CHANNEL: &'static str = BATTERY_STATE_CHANNEL;
}

/// Reader for the battery snapshot the platform publishes.
pub type BatteryStateClient = LatestClient<BatteryState>;

/// Battery snapshots as they are published.
pub type BatteryStateStream = EarlyStream<BatteryState>;

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq)]
pub struct BatteryState {
    /// Fractional charge `0.0..=1.0`. `None` on devices without a
    /// battery (desktops wired to mains, platforms that refused to
    /// expose a reading).
    pub level: Option<f32>,

    pub charging: ChargingState,

    pub power_source: PowerSource,

    /// Low-power / battery-saver mode enabled by the user.
    pub power_saver: bool,

    pub thermal_state: ThermalState,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChargingState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    NotCharging,
    Full,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PowerSource {
    #[default]
    Unknown,
    Battery,
    Ac,
    Usb,
    Wireless,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThermalState {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}
