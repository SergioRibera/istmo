//! Pure-Rust desktop sampler for
//! [`PowerDevicesClient`](super::power_devices::PowerDevicesClient).
//!
//! - Linux asks `UPower` over the system D-Bus (`EnumerateDevices`, then
//!   each device's properties) and falls back to
//!   `/sys/class/power_supply` when `UPower` does not answer.
//! - macOS reads the `IOKit` power sources (internal batteries, UPS),
//!   the `IOKit` HID accessories with a `BatteryPercent` (Magic Mouse,
//!   Keyboard, Trackpad) and `GCController` batteries.
//! - Windows reports the system battery (`GetSystemPowerStatus`) and
//!   every present device with the Bluetooth battery property Windows
//!   shows in Settings, classified by its Bluetooth class of device.
//!
//! Call [`PowerDevicesPublisher::tick`] on an interval (every ~30 s) or
//! when the app learns a device came or went; the sampler publishes only
//! when the list changes.

use super::power_devices::PowerDevices;
use super::power_devices::{PowerDevice, PowerDeviceKind};
#[cfg(any(target_os = "linux", target_os = "macos", test))]
use crate::battery_state::ChargingState;
use crate::early::LatestPublisher;

/// Publishes every battery-backed device of the host.
pub type PowerDevicesPublisher = LatestPublisher<PowerDevices>;

impl LatestPublisher<PowerDevices> {
    /// Sample the host and publish if the device list changed.
    pub fn tick(&mut self) {
        if let Some(mut devices) = sample() {
            devices.sort_by(|a, b| a.id.cmp(&b.id));
            self.publish_if_changed(PowerDevices { devices });
        }
    }
}

fn sample() -> Option<Vec<PowerDevice>> {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(target_os = "macos")]
    {
        macos::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
}

/// Kind of a peripheral from its product or driver name, for platforms
/// that report no device type.
fn kind_from_name(name: &str) -> PowerDeviceKind {
    let haystack = name.to_ascii_lowercase();
    let has = |needles: &[&str]| needles.iter().any(|needle| haystack.contains(needle));
    if has(&["mouse", "trackball"]) {
        PowerDeviceKind::Mouse
    } else if has(&["trackpad", "touchpad"]) {
        PowerDeviceKind::Touchpad
    } else if has(&["keyboard", "kbd"]) {
        PowerDeviceKind::Keyboard
    } else if has(&[
        "controller",
        "gamepad",
        "joy-con",
        "joycon",
        "dualsense",
        "dualshock",
        "xpad",
    ]) {
        PowerDeviceKind::Gamepad
    } else if has(&["wacom", "stylus", "pencil", "pen"]) {
        PowerDeviceKind::Stylus
    } else if has(&["headset", "headphone", "earbud", "airpods", "buds"]) {
        PowerDeviceKind::Headphones
    } else if has(&["speaker"]) {
        PowerDeviceKind::Speaker
    } else {
        PowerDeviceKind::Other
    }
}

/// `level` from an `IOKit` current / max capacity pair.
#[cfg(any(target_os = "macos", test))]
#[expect(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    reason = "capacities are percentages and a ratio in 0..=1 fits f32"
)]
fn capacity_level(current: Option<i64>, max: Option<i64>) -> Option<f32> {
    let (current, max) = (current?, max?);
    (max > 0).then(|| (current.clamp(0, max) as f64 / max as f64) as f32)
}

/// Charging state of an `IOKit` power source.
#[cfg(any(target_os = "macos", test))]
const fn iops_charging(
    charging: Option<bool>,
    charged: Option<bool>,
    on_ac: Option<bool>,
) -> ChargingState {
    match (charging, charged, on_ac) {
        (Some(true), _, _) => ChargingState::Charging,
        (_, Some(true), _) => ChargingState::Full,
        (_, _, Some(true)) => ChargingState::NotCharging,
        (_, _, Some(false)) => ChargingState::Discharging,
        _ => ChargingState::Unknown,
    }
}

/// `GCDeviceBatteryState`: -1 unknown, 0 discharging, 1 charging,
/// 2 full.
#[cfg(any(target_os = "macos", test))]
const fn game_controller_charging(state: isize) -> ChargingState {
    match state {
        0 => ChargingState::Discharging,
        1 => ChargingState::Charging,
        2 => ChargingState::Full,
        _ => ChargingState::Unknown,
    }
}

/// Kind from a Bluetooth class of device (Assigned Numbers, "Class of
/// Device"): major class in bits 8..13, minor class in bits 2..8.
#[cfg(any(target_os = "windows", test))]
const fn class_of_device_kind(class_of_device: u32) -> Option<PowerDeviceKind> {
    let major = (class_of_device >> 8) & 0x1F;
    let minor = (class_of_device >> 2) & 0x3F;
    Some(match major {
        0x02 => PowerDeviceKind::Phone,
        // Audio / video: wearable headset, hands-free, headphones.
        0x04 => match minor {
            0x01 | 0x02 | 0x06 => PowerDeviceKind::Headphones,
            0x05 => PowerDeviceKind::Speaker,
            _ => PowerDeviceKind::Other,
        },
        // Peripheral: device type in the low four minor bits, keyboard /
        // pointing flags in the top two.
        0x05 => match (minor & 0x0F, minor >> 4) {
            (0x01 | 0x02, _) => PowerDeviceKind::Gamepad,
            (0x03, _) => PowerDeviceKind::Remote,
            (0x05, _) => PowerDeviceKind::Tablet,
            (0x07, _) => PowerDeviceKind::Stylus,
            (_, 0x01 | 0x03) => PowerDeviceKind::Keyboard,
            (_, 0x02) => PowerDeviceKind::Mouse,
            _ => return None,
        },
        0x07 => PowerDeviceKind::Wearable,
        _ => return None,
    })
}

/// The device's own battery, from the aggregate battery sampler.
#[cfg(any(target_os = "windows", test))]
fn system_battery(state: &crate::battery_state::BatteryState) -> Option<PowerDevice> {
    state.level.map(|level| PowerDevice {
        id: "system".to_owned(),
        name: None,
        kind: PowerDeviceKind::Internal,
        level: Some(level),
        charging: state.charging,
    })
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::{
        PowerDevice, PowerDeviceKind, capacity_level, game_controller_charging, iops_charging,
        kind_from_name,
    };
    use crate::macos_power::{hid_batteries, power_sources, string};

    // Loads the framework so `GCController` resolves.
    #[link(name = "GameController", kind = "framework")]
    unsafe extern "C" {}

    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the `Option` signature of the other platforms' samplers"
    )]
    pub(super) fn sample() -> Option<Vec<PowerDevice>> {
        let mut devices = Vec::new();
        if let Some(snapshot) = power_sources() {
            devices.extend(
                snapshot
                    .sources
                    .into_iter()
                    .filter(|s| s.present)
                    .map(|source| PowerDevice {
                        id: source.id.map_or_else(
                            || format!("iops:{}", source.name.as_deref().unwrap_or_default()),
                            |id| format!("iops:{id}"),
                        ),
                        kind: match source.kind.as_str() {
                            "InternalBattery" => PowerDeviceKind::Internal,
                            "UPS" => PowerDeviceKind::Ups,
                            _ => kind_from_name(source.name.as_deref().unwrap_or_default()),
                        },
                        level: capacity_level(source.current, source.max),
                        charging: iops_charging(source.charging, source.charged, source.on_ac),
                        name: source.name,
                    }),
            );
        }
        devices.extend(hid_batteries().into_iter().map(|battery| PowerDevice {
            id: format!("ioreg:{}", battery.registry_id),
            kind: kind_from_name(battery.product.as_deref().unwrap_or_default()),
            level: capacity_level(Some(battery.percent), Some(100)),
            charging: crate::battery_state::ChargingState::Unknown,
            name: battery.product,
        }));
        devices.extend(game_controllers());
        Some(devices)
    }

    /// `GCController.controllers` with their `GCDeviceBattery`
    /// (macOS 11+).
    fn game_controllers() -> Vec<PowerDevice> {
        let Some(class) = AnyClass::get(c"GCController") else {
            return Vec::new();
        };
        objc2::rc::autoreleasepool(|_| {
            // SAFETY: GameController API on live objects; `battery` is
            // only sent after `respondsToSelector:` confirms it, and may
            // be nil for controllers without a readable battery.
            unsafe {
                let controllers: *const AnyObject = msg_send![class, controllers];
                if controllers.is_null() {
                    return Vec::new();
                }
                let count: usize = msg_send![controllers, count];
                (0..count)
                    .map(|index| {
                        let controller: *const AnyObject =
                            msg_send![controllers, objectAtIndex: index];
                        let has_battery: bool = msg_send![
                            controller,
                            respondsToSelector: objc2::sel!(battery)
                        ];
                        let battery: *const AnyObject = if has_battery {
                            msg_send![controller, battery]
                        } else {
                            std::ptr::null()
                        };
                        let (level, charging) = if battery.is_null() {
                            (None, game_controller_charging(-1))
                        } else {
                            let level: f32 = msg_send![battery, batteryLevel];
                            let state: isize = msg_send![battery, batteryState];
                            (Some(level.clamp(0.0, 1.0)), game_controller_charging(state))
                        };
                        let vendor: *const AnyObject = msg_send![controller, vendorName];
                        PowerDevice {
                            // The controller object lives as long as the
                            // connection, so its address names it.
                            id: format!("gc:{:x}", controller as usize),
                            name: string(vendor),
                            kind: PowerDeviceKind::Gamepad,
                            level,
                            charging,
                        }
                    })
                    .collect()
            }
        })
    }
}

#[cfg(target_os = "windows")]
mod windows {
    use windows_sys::Win32::Devices::DeviceAndDriverInstallation::{
        DIGCF_ALLCLASSES, DIGCF_PRESENT, HDEVINFO, SP_DEVINFO_DATA, SetupDiDestroyDeviceInfoList,
        SetupDiEnumDeviceInfo, SetupDiGetClassDevsW, SetupDiGetDeviceInstanceIdW,
        SetupDiGetDevicePropertyW,
    };
    use windows_sys::Win32::Devices::Properties::{
        DEVPKEY_Device_FriendlyName, DEVPKEY_NAME, DEVPROP_TYPE_BYTE, DEVPROP_TYPE_STRING,
        DEVPROP_TYPE_UINT32, DEVPROPTYPE,
    };
    use windows_sys::Win32::Foundation::DEVPROPKEY;
    use windows_sys::core::GUID;

    use super::{PowerDevice, class_of_device_kind, kind_from_name, system_battery};
    use crate::battery_state::ChargingState;

    /// `DEVPKEY_Bluetooth_Battery`: the level Settings shows, in percent.
    const BLUETOOTH_BATTERY: DEVPROPKEY = DEVPROPKEY {
        fmtid: GUID::from_u128(0x104e_a319_6ee2_4701_bd47_8ddb_f425_bbe5),
        pid: 2,
    };
    /// `DEVPKEY_Bluetooth_ClassOfDevice`.
    const BLUETOOTH_CLASS_OF_DEVICE: DEVPROPKEY = DEVPROPKEY {
        fmtid: GUID::from_u128(0x2bd6_7d8b_8beb_48d5_87e0_6cda_3428_040a),
        pid: 10,
    };
    const INVALID_DEVICE_INFO_SET: HDEVINFO = -1;

    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the `Option` signature of the other platforms' samplers"
    )]
    pub(super) fn sample() -> Option<Vec<PowerDevice>> {
        let mut devices: Vec<PowerDevice> = crate::battery_state_publisher::host_sample()
            .as_ref()
            .and_then(system_battery)
            .into_iter()
            .collect();
        devices.extend(bluetooth_batteries());
        Some(devices)
    }

    fn bluetooth_batteries() -> Vec<PowerDevice> {
        // SAFETY: enumerates every present device; the set is destroyed
        // below and `info` is a correctly sized SP_DEVINFO_DATA.
        unsafe {
            let set = SetupDiGetClassDevsW(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null_mut(),
                DIGCF_ALLCLASSES | DIGCF_PRESENT,
            );
            if set == INVALID_DEVICE_INFO_SET {
                return Vec::new();
            }
            let mut out = Vec::new();
            let mut info: SP_DEVINFO_DATA = std::mem::zeroed();
            info.cbSize = u32::try_from(size_of::<SP_DEVINFO_DATA>()).unwrap_or(u32::MAX);
            let mut index = 0;
            while SetupDiEnumDeviceInfo(set, index, &raw mut info) != 0 {
                index += 1;
                let Some(percent) = property(set, &info, &BLUETOOTH_BATTERY, DEVPROP_TYPE_BYTE)
                    .and_then(|bytes| bytes.first().copied())
                else {
                    continue;
                };
                let name = string_property(set, &info, &DEVPKEY_Device_FriendlyName)
                    .or_else(|| string_property(set, &info, &DEVPKEY_NAME));
                let kind = property(set, &info, &BLUETOOTH_CLASS_OF_DEVICE, DEVPROP_TYPE_UINT32)
                    .and_then(|bytes| bytes.get(..4)?.try_into().ok())
                    .and_then(|bytes| class_of_device_kind(u32::from_le_bytes(bytes)))
                    .unwrap_or_else(|| kind_from_name(name.as_deref().unwrap_or_default()));
                out.push(PowerDevice {
                    id: instance_id(set, &info).unwrap_or_else(|| format!("bluetooth:{index}")),
                    name,
                    kind,
                    level: Some(f32::from(percent.min(100)) / 100.0),
                    charging: ChargingState::Unknown,
                });
            }
            SetupDiDestroyDeviceInfoList(set);
            out
        }
    }

    /// Raw bytes of a device property of type `expected`.
    ///
    /// # Safety
    ///
    /// `set` is a live device information set and `info` one of its
    /// members.
    unsafe fn property(
        set: HDEVINFO,
        info: &SP_DEVINFO_DATA,
        key: &DEVPROPKEY,
        expected: DEVPROPTYPE,
    ) -> Option<Vec<u8>> {
        let mut kind: DEVPROPTYPE = 0;
        let mut size = 0_u32;
        // SAFETY: a sizing call (NULL buffer), then a fill call with a
        // buffer of the size the first one reported.
        unsafe {
            SetupDiGetDevicePropertyW(
                set,
                info,
                key,
                &raw mut kind,
                std::ptr::null_mut(),
                0,
                &raw mut size,
                0,
            );
            if size == 0 || kind != expected {
                return None;
            }
            let mut buffer = vec![0_u8; size as usize];
            let ok = SetupDiGetDevicePropertyW(
                set,
                info,
                key,
                &raw mut kind,
                buffer.as_mut_ptr(),
                size,
                &raw mut size,
                0,
            );
            (ok != 0 && kind == expected).then_some(buffer)
        }
    }

    /// # Safety
    ///
    /// As [`property`].
    unsafe fn string_property(
        set: HDEVINFO,
        info: &SP_DEVINFO_DATA,
        key: &DEVPROPKEY,
    ) -> Option<String> {
        // SAFETY: forwarded from the caller.
        let bytes = unsafe { property(set, info, key, DEVPROP_TYPE_STRING) }?;
        let wide: Vec<u16> = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_le_bytes(*pair))
            .take_while(|&unit| unit != 0)
            .collect();
        let text = String::from_utf16_lossy(&wide);
        (!text.trim().is_empty()).then_some(text)
    }

    /// # Safety
    ///
    /// As [`property`].
    unsafe fn instance_id(set: HDEVINFO, info: &SP_DEVINFO_DATA) -> Option<String> {
        // Device instance ids are at most 200 characters.
        let mut buffer = [0_u16; 256];
        let mut required = 0_u32;
        // SAFETY: the buffer holds `buffer.len()` UTF-16 units.
        let ok = unsafe {
            SetupDiGetDeviceInstanceIdW(
                set,
                info,
                buffer.as_mut_ptr(),
                u32::try_from(buffer.len()).unwrap_or(u32::MAX),
                &raw mut required,
            )
        };
        if ok == 0 {
            return None;
        }
        let len = buffer
            .iter()
            .position(|&unit| unit == 0)
            .unwrap_or(buffer.len());
        Some(String::from_utf16_lossy(&buffer[..len]))
    }
}

/// The `org.freedesktop.UPower.Device` properties the mapping reads.
#[cfg(any(target_os = "linux", test))]
#[derive(Debug, Clone, Default)]
struct UpowerDevice {
    object_path: String,
    native_path: String,
    vendor: String,
    model: String,
    /// `Type`: 1 line power, 2 battery, 3 UPS, 5 mouse, …
    kind: u32,
    /// The device powers the system (internal batteries).
    power_supply: bool,
    /// `0.0..=100.0`.
    percentage: f64,
    /// 1 charging, 2 discharging, 3 empty, 4 full, 5/6 pending.
    state: u32,
    is_present: bool,
}

#[cfg(any(target_os = "linux", test))]
impl UpowerDevice {
    const LINE_POWER: u32 = 1;
    const BATTERY: u32 = 2;
    const STATE_EMPTY: u32 = 3;

    /// `None` for line power and absent battery slots.
    fn to_power_device(&self) -> Option<PowerDevice> {
        if self.kind == Self::LINE_POWER || (self.kind == Self::BATTERY && !self.is_present) {
            return None;
        }
        let kind = match self.kind {
            Self::BATTERY if self.power_supply => PowerDeviceKind::Internal,
            3 => PowerDeviceKind::Ups,
            5 => PowerDeviceKind::Mouse,
            6 => PowerDeviceKind::Keyboard,
            8 => PowerDeviceKind::Phone,
            10 => PowerDeviceKind::Tablet,
            12 => PowerDeviceKind::Gamepad,
            13 => PowerDeviceKind::Stylus,
            14 => PowerDeviceKind::Touchpad,
            17 | 19 => PowerDeviceKind::Headphones,
            18 => PowerDeviceKind::Speaker,
            22 => PowerDeviceKind::Remote,
            26 => PowerDeviceKind::Wearable,
            _ => PowerDeviceKind::Other,
        };
        // A level of exactly 0 is "unknown" unless the device says empty.
        #[expect(clippy::cast_possible_truncation, reason = "a ratio in 0..=1 fits f32")]
        let level = (self.percentage > 0.0 || self.state == Self::STATE_EMPTY)
            .then(|| (self.percentage.clamp(0.0, 100.0) / 100.0) as f32);
        let charging = match self.state {
            1 => ChargingState::Charging,
            2 | 3 => ChargingState::Discharging,
            4 => ChargingState::Full,
            5 | 6 => ChargingState::NotCharging,
            _ => ChargingState::Unknown,
        };
        let name = [&self.model, &self.vendor]
            .into_iter()
            .map(|s| s.trim())
            .find(|s| !s.is_empty())
            .map(str::to_owned);
        let id = if self.native_path.is_empty() {
            &self.object_path
        } else {
            &self.native_path
        };
        Some(PowerDevice {
            id: id.clone(),
            name,
            kind,
            level,
            charging,
        })
    }
}

/// Kind of a `/sys/class/power_supply` entry. `scope` is `Device` for
/// peripherals; the kernel has no kind attribute, so peripherals are
/// classified from their supply and model names.
#[cfg(any(target_os = "linux", test))]
fn sysfs_kind(supply_type: &str, scope: &str, name: &str, model: &str) -> Option<PowerDeviceKind> {
    match supply_type {
        "UPS" => return Some(PowerDeviceKind::Ups),
        "Battery" => {}
        // Mains, USB, Wireless: chargers, not batteries.
        _ => return None,
    }
    if scope != "Device" {
        return Some(PowerDeviceKind::Internal);
    }
    Some(kind_from_name(&format!("{name} {model}")))
}

/// Level of a sysfs entry: `capacity` when present, else the coarse
/// `capacity_level` some HID drivers report.
#[cfg(any(target_os = "linux", test))]
fn sysfs_level(capacity: Option<u8>, capacity_level: &str) -> Option<f32> {
    if let Some(percent) = capacity {
        return Some(f32::from(percent.min(100)) / 100.0);
    }
    match capacity_level {
        "Full" => Some(1.0),
        "High" => Some(0.8),
        "Normal" => Some(0.5),
        "Low" => Some(0.2),
        "Critical" => Some(0.05),
        _ => None,
    }
}

#[cfg(any(target_os = "linux", test))]
fn sysfs_charging(status: &str) -> ChargingState {
    match status {
        "Charging" => ChargingState::Charging,
        "Discharging" => ChargingState::Discharging,
        "Not charging" => ChargingState::NotCharging,
        "Full" => ChargingState::Full,
        _ => ChargingState::Unknown,
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::collections::HashMap;
    use std::fs;
    use std::path::Path;
    use std::sync::Mutex;

    use istmo_core::sync::lock;
    use zbus::blocking::Connection;
    use zbus::zvariant::{OwnedObjectPath, OwnedValue};

    use super::{PowerDevice, UpowerDevice, sysfs_charging, sysfs_kind, sysfs_level};

    const UPOWER: &str = "org.freedesktop.UPower";
    const UPOWER_PATH: &str = "/org/freedesktop/UPower";
    const DEVICE_INTERFACE: &str = "org.freedesktop.UPower.Device";
    const PROPERTIES: &str = "org.freedesktop.DBus.Properties";
    const POWER_SUPPLY: &str = "/sys/class/power_supply";

    /// The system bus, opened on the first tick and kept: connecting is
    /// the expensive part of a sample.
    static BUS: Mutex<Option<Connection>> = Mutex::new(None);

    #[expect(
        clippy::unnecessary_wraps,
        reason = "shares the `Option` signature of the other platforms' samplers"
    )]
    pub(super) fn sample() -> Option<Vec<PowerDevice>> {
        match upower() {
            Ok(devices) => Some(devices),
            Err(err) => {
                tracing::debug!(target: "istmo.power_devices", %err, "UPower unavailable, reading sysfs");
                // A dropped bus is reopened on the next tick.
                lock(&BUS).take();
                Some(sysfs())
            }
        }
    }

    fn upower() -> zbus::Result<Vec<PowerDevice>> {
        let bus = {
            let mut bus = lock(&BUS);
            if let Some(bus) = bus.as_ref() {
                bus.clone()
            } else {
                let opened = Connection::system()?;
                *bus = Some(opened.clone());
                opened
            }
        };
        let paths: Vec<OwnedObjectPath> = bus
            .call_method(
                Some(UPOWER),
                UPOWER_PATH,
                Some(UPOWER),
                "EnumerateDevices",
                &(),
            )?
            .body()
            .deserialize()?;
        let mut devices = Vec::new();
        for path in paths {
            let properties: HashMap<String, OwnedValue> = bus
                .call_method(
                    Some(UPOWER),
                    path.as_str(),
                    Some(PROPERTIES),
                    "GetAll",
                    &(DEVICE_INTERFACE,),
                )?
                .body()
                .deserialize()?;
            let read = |key: &str| properties.get(key);
            let string = |key: &str| {
                read(key)
                    .and_then(|value| value.downcast_ref::<&str>().ok())
                    .unwrap_or_default()
                    .to_owned()
            };
            let device = UpowerDevice {
                object_path: path.as_str().to_owned(),
                native_path: string("NativePath"),
                vendor: string("Vendor"),
                model: string("Model"),
                kind: read("Type")
                    .and_then(|value| value.downcast_ref::<u32>().ok())
                    .unwrap_or_default(),
                power_supply: read("PowerSupply")
                    .and_then(|value| value.downcast_ref::<bool>().ok())
                    .unwrap_or_default(),
                percentage: read("Percentage")
                    .and_then(|value| value.downcast_ref::<f64>().ok())
                    .unwrap_or_default(),
                state: read("State")
                    .and_then(|value| value.downcast_ref::<u32>().ok())
                    .unwrap_or_default(),
                is_present: read("IsPresent")
                    .and_then(|value| value.downcast_ref::<bool>().ok())
                    .unwrap_or(true),
            };
            devices.extend(device.to_power_device());
        }
        Ok(devices)
    }

    fn sysfs() -> Vec<PowerDevice> {
        let Ok(entries) = fs::read_dir(POWER_SUPPLY) else {
            return Vec::new();
        };
        entries
            .flatten()
            .filter_map(|entry| {
                let dir = entry.path();
                let name = entry.file_name().to_string_lossy().into_owned();
                let model = read(&dir, "model_name");
                let kind = sysfs_kind(&read(&dir, "type"), &read(&dir, "scope"), &name, &model)?;
                if read(&dir, "present") == "0" {
                    return None;
                }
                let manufacturer = read(&dir, "manufacturer");
                Some(PowerDevice {
                    name: [model, manufacturer].into_iter().find(|s| !s.is_empty()),
                    kind,
                    level: sysfs_level(
                        read(&dir, "capacity").parse().ok(),
                        &read(&dir, "capacity_level"),
                    ),
                    charging: sysfs_charging(&read(&dir, "status")),
                    id: name,
                })
            })
            .collect()
    }

    fn read(dir: &Path, attribute: &str) -> String {
        fs::read_to_string(dir.join(attribute))
            .map(|value| value.trim().to_owned())
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upower(kind: u32) -> UpowerDevice {
        UpowerDevice {
            object_path: "/org/freedesktop/UPower/devices/x".to_owned(),
            native_path: "hidpp_battery_0".to_owned(),
            vendor: "Logitech".to_owned(),
            model: "MX Master 3".to_owned(),
            kind,
            power_supply: false,
            percentage: 55.0,
            state: 2,
            is_present: true,
        }
    }

    #[test]
    fn upower_peripherals_and_internal_batteries() {
        let mouse = upower(5).to_power_device().unwrap();
        assert_eq!(mouse.id, "hidpp_battery_0");
        assert_eq!(mouse.name.as_deref(), Some("MX Master 3"));
        assert_eq!(mouse.kind, PowerDeviceKind::Mouse);
        assert_eq!(mouse.level, Some(0.55));
        assert_eq!(mouse.charging, ChargingState::Discharging);

        let internal = UpowerDevice {
            power_supply: true,
            state: 4,
            percentage: 100.0,
            ..upower(2)
        };
        let internal = internal.to_power_device().unwrap();
        assert_eq!(internal.kind, PowerDeviceKind::Internal);
        assert_eq!(internal.charging, ChargingState::Full);
        assert_eq!(
            upower(2).to_power_device().unwrap().kind,
            PowerDeviceKind::Other
        );
        assert_eq!(
            upower(17).to_power_device().unwrap().kind,
            PowerDeviceKind::Headphones
        );
        assert_eq!(
            upower(12).to_power_device().unwrap().kind,
            PowerDeviceKind::Gamepad
        );
    }

    #[test]
    fn upower_skips_line_power_and_empty_slots() {
        assert!(upower(1).to_power_device().is_none());
        let absent = UpowerDevice {
            is_present: false,
            ..upower(2)
        };
        assert!(absent.to_power_device().is_none());
    }

    #[test]
    fn upower_unknown_level_and_fallback_ids() {
        let unknown = UpowerDevice {
            percentage: 0.0,
            state: 0,
            native_path: String::new(),
            model: " ".to_owned(),
            ..upower(19)
        };
        let device = unknown.to_power_device().unwrap();
        assert_eq!(device.level, None);
        assert_eq!(device.id, "/org/freedesktop/UPower/devices/x");
        assert_eq!(device.name.as_deref(), Some("Logitech"));
        let empty = UpowerDevice {
            percentage: 0.0,
            state: 3,
            ..upower(6)
        };
        assert_eq!(empty.to_power_device().unwrap().level, Some(0.0));
    }

    #[test]
    fn sysfs_classifies_supplies() {
        assert_eq!(sysfs_kind("Mains", "", "AC", ""), None);
        assert_eq!(
            sysfs_kind("Battery", "", "BAT0", ""),
            Some(PowerDeviceKind::Internal)
        );
        assert_eq!(
            sysfs_kind("Battery", "System", "BAT1", ""),
            Some(PowerDeviceKind::Internal)
        );
        assert_eq!(sysfs_kind("UPS", "", "ups", ""), Some(PowerDeviceKind::Ups));
        assert_eq!(
            sysfs_kind("Battery", "Device", "hidpp_battery_0", "MX Master 3 Mouse"),
            Some(PowerDeviceKind::Mouse)
        );
        assert_eq!(
            sysfs_kind("Battery", "Device", "ps-controller-battery-aa:bb", ""),
            Some(PowerDeviceKind::Gamepad)
        );
        assert_eq!(
            sysfs_kind("Battery", "Device", "wacom_battery_0", ""),
            Some(PowerDeviceKind::Stylus)
        );
        assert_eq!(
            sysfs_kind("Battery", "Device", "hid-xx-battery", "Thing"),
            Some(PowerDeviceKind::Other)
        );
    }

    #[test]
    fn sysfs_levels_and_status() {
        assert_eq!(sysfs_level(Some(42), ""), Some(0.42));
        assert_eq!(sysfs_level(Some(250), ""), Some(1.0));
        assert_eq!(sysfs_level(None, "Low"), Some(0.2));
        assert_eq!(sysfs_level(None, "Unknown"), None);
        assert_eq!(sysfs_charging("Not charging"), ChargingState::NotCharging);
        assert_eq!(sysfs_charging("weird"), ChargingState::Unknown);
    }

    #[test]
    fn names_classify_peripherals() {
        assert_eq!(kind_from_name("Magic Mouse"), PowerDeviceKind::Mouse);
        assert_eq!(kind_from_name("Magic Trackpad"), PowerDeviceKind::Touchpad);
        assert_eq!(kind_from_name("Magic Keyboard"), PowerDeviceKind::Keyboard);
        assert_eq!(
            kind_from_name("DualSense Wireless Controller"),
            PowerDeviceKind::Gamepad
        );
        assert_eq!(kind_from_name("AirPods Pro"), PowerDeviceKind::Headphones);
        assert_eq!(kind_from_name("Thing"), PowerDeviceKind::Other);
    }

    #[test]
    fn iokit_levels_and_charging() {
        assert_eq!(capacity_level(Some(42), Some(100)), Some(0.42));
        assert_eq!(capacity_level(Some(42), Some(0)), None);
        assert_eq!(capacity_level(None, Some(100)), None);
        assert_eq!(
            iops_charging(Some(true), None, Some(true)),
            ChargingState::Charging
        );
        assert_eq!(
            iops_charging(Some(false), Some(true), Some(true)),
            ChargingState::Full
        );
        assert_eq!(
            iops_charging(Some(false), None, Some(true)),
            ChargingState::NotCharging
        );
        assert_eq!(
            iops_charging(Some(false), None, Some(false)),
            ChargingState::Discharging
        );
        assert_eq!(iops_charging(None, None, None), ChargingState::Unknown);
        assert_eq!(game_controller_charging(1), ChargingState::Charging);
        assert_eq!(game_controller_charging(-1), ChargingState::Unknown);
    }

    #[test]
    fn bluetooth_class_of_device() {
        // Audio/video headphones (0x240418), headset (0x200404),
        // loudspeaker (0x240414).
        assert_eq!(
            class_of_device_kind(0x0024_0418),
            Some(PowerDeviceKind::Headphones)
        );
        assert_eq!(
            class_of_device_kind(0x0020_0404),
            Some(PowerDeviceKind::Headphones)
        );
        assert_eq!(
            class_of_device_kind(0x0024_0414),
            Some(PowerDeviceKind::Speaker)
        );
        // Peripheral: keyboard (0x2540), mouse (0x2580), gamepad (0x2508).
        assert_eq!(
            class_of_device_kind(0x0000_2540),
            Some(PowerDeviceKind::Keyboard)
        );
        assert_eq!(
            class_of_device_kind(0x0000_2580),
            Some(PowerDeviceKind::Mouse)
        );
        assert_eq!(
            class_of_device_kind(0x0000_2508),
            Some(PowerDeviceKind::Gamepad)
        );
        // Phone (smartphone 0x5a020c), wearable (wrist watch 0x0704).
        assert_eq!(
            class_of_device_kind(0x005a_020c),
            Some(PowerDeviceKind::Phone)
        );
        assert_eq!(
            class_of_device_kind(0x0000_0704),
            Some(PowerDeviceKind::Wearable)
        );
        // Computer: not classified.
        assert_eq!(class_of_device_kind(0x0000_010c), None);
    }

    #[test]
    fn system_battery_only_with_a_level() {
        let mut state = crate::battery_state::BatteryState {
            level: Some(0.5),
            charging: ChargingState::Charging,
            power_source: crate::battery_state::PowerSource::Ac,
            power_saver: false,
            thermal_state: crate::battery_state::ThermalState::Nominal,
        };
        let device = system_battery(&state).unwrap();
        assert_eq!(device.kind, PowerDeviceKind::Internal);
        assert_eq!(device.charging, ChargingState::Charging);
        state.level = None;
        assert!(system_battery(&state).is_none());
    }
}
