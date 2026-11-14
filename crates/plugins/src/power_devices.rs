//! Battery of every power source the system knows about: the device's
//! own batteries, a UPS, and connected peripherals (mice, keyboards,
//! headphones, controllers, styluses, …).
//!
//! [`BatteryStateClient`](crate::BatteryStateClient) stays the cheap,
//! permission-free summary of the device itself; this stream is the
//! full list. Coverage depends on what each platform exposes:
//!
//! - Linux: `PowerDevicesPublisher` reads `UPower` over D-Bus (every
//!   battery `UPower` tracks, Bluetooth audio included) and falls back to
//!   `/sys/class/power_supply` when `UPower` is not running.
//! - macOS: `PowerDevicesPublisher` reads `IOKit` power sources
//!   (internal batteries, UPS), the Bluetooth HID accessories `IOKit`
//!   reports a `BatteryPercent` for (Magic Mouse, Keyboard, Trackpad)
//!   and `GameController` batteries.
//! - Windows: `PowerDevicesPublisher` reads the system battery and the
//!   battery level Windows keeps for Bluetooth devices.
//! - Android (Kotlin runtime): the device battery and, on API 31+, the
//!   batteries of input devices (controllers, styluses). Other
//!   Bluetooth batteries are not public API.
//! - iOS (Swift runtime): the device battery and `GameController`
//!   batteries.
//!
//! Devices without a readable level still appear, with `level: None`.

use istmo_macros::message;

use crate::battery_state::ChargingState;
use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the power devices stream.
pub const POWER_DEVICES_PLUGIN_ID: &str = "istmo.power_devices";
/// Early-event channel the device list is published on.
pub const POWER_DEVICES_CHANNEL: &str = "istmo.power_devices";

/// Every battery-backed device the platform reports, in a stable order
/// (by [`PowerDevice::id`]).
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PowerDevices {
    /// The devices, possibly none.
    pub devices: Vec<PowerDevice>,
}

/// One battery-backed device.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq)]
pub struct PowerDevice {
    /// Platform identifier, stable while the device stays connected
    /// (`UPower` native path, `IOKit` registry id, Windows device
    /// instance id, …). Use it to match a device across updates.
    pub id: String,
    /// Product name when the platform reports one.
    pub name: Option<String>,
    /// What kind of device it is.
    pub kind: PowerDeviceKind,
    /// Fractional charge `0.0..=1.0`, `None` when unknown.
    pub level: Option<f32>,
    /// Charging state, [`ChargingState::Unknown`] when the platform
    /// does not say.
    pub charging: ChargingState,
}

/// Kind of a [`PowerDevice`].
///
/// New variants are only ever appended, so existing wire indices stay
/// stable.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum PowerDeviceKind {
    /// Anything else, or the platform did not say.
    #[default]
    Other,
    /// A battery that powers this device.
    Internal,
    /// Uninterruptible power supply.
    Ups,
    /// Mouse or trackball.
    Mouse,
    /// Keyboard.
    Keyboard,
    /// Touchpad / trackpad.
    Touchpad,
    /// Game controller.
    Gamepad,
    /// Stylus or pen.
    Stylus,
    /// Headphones, earbuds or headset.
    Headphones,
    /// Speaker.
    Speaker,
    /// Phone.
    Phone,
    /// Tablet (including drawing tablets).
    Tablet,
    /// Watch or other wearable.
    Wearable,
    /// Remote control.
    Remote,
}

impl LatestValue for PowerDevices {
    const PLUGIN_ID: &'static str = POWER_DEVICES_PLUGIN_ID;
    const CHANNEL: &'static str = POWER_DEVICES_CHANNEL;
}

/// Reader for the device list the platform publishes.
pub type PowerDevicesClient = LatestClient<PowerDevices>;

/// Device lists as they are published.
pub type PowerDevicesStream = EarlyStream<PowerDevices>;
