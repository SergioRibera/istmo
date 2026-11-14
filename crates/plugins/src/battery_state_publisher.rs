//! Pure-Rust desktop sampler for
//! [`BatteryStateClient`](super::battery_state::BatteryStateClient).
//!
//! - Linux reads `/sys/class/power_supply/BAT*/` sysfs attributes.
//! - Windows calls `GetSystemPowerStatus` via `windows-sys`.
//! - macOS reads the internal batteries from `IOKit`
//!   (`IOPSCopyPowerSourcesInfo`), low power mode and thermal state from
//!   `NSProcessInfo`.
//!
//! Call [`BatteryStatePublisher::tick`] on an interval (every ~15s) or
//! on power events the surrounding app observes — the sampler publishes
//! only when the snapshot actually changes.

use super::battery_state::BatteryState;
#[cfg(any(target_os = "linux", target_os = "windows", target_os = "macos", test))]
use super::battery_state::{ChargingState, PowerSource, ThermalState};
use crate::early::LatestPublisher;

/// Publishes the host's battery snapshot.
pub type BatteryStatePublisher = LatestPublisher<BatteryState>;

impl LatestPublisher<BatteryState> {
    /// Sample the host and publish if the snapshot changed.
    pub fn tick(&mut self) {
        if let Some(sample) = host_sample() {
            self.publish_if_changed(sample);
        }
    }
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows", target_os = "macos")),
    expect(
        clippy::missing_const_for_fn,
        reason = "only the unsupported-target body is const"
    )
)]
/// The host's battery snapshot, as `tick` publishes it.
pub(crate) fn host_sample() -> Option<BatteryState> {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
    #[cfg(target_os = "macos")]
    {
        macos::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        None
    }
}

/// Fold the per-cell readings of a (possibly multi-battery) machine into
/// one snapshot. Dual-battery laptops (`BAT0` + `BAT1`) expose
/// one sysfs entry per cell: capacities are averaged (`energy_now` is
/// not universally present) and any charging cell makes the whole
/// machine charging.
#[cfg(any(target_os = "linux", test))]
fn aggregate_cells(capacities: &[u8], statuses: &[&str], ac_online: bool) -> BatteryState {
    let level = u32::try_from(capacities.len())
        .ok()
        .filter(|&n| n > 0)
        .map(|n| {
            let sum: u32 = capacities.iter().map(|&c| u32::from(c.min(100))).sum();
            // Tenths of a percent fit a u16 (at most 1000).
            let tenths = u16::try_from(sum * 10 / n).unwrap_or(1000);
            f32::from(tenths) / 1000.0
        });
    let charging = if statuses.contains(&"Charging") {
        ChargingState::Charging
    } else if !statuses.is_empty() && statuses.iter().all(|&s| s == "Full") {
        ChargingState::Full
    } else if statuses.contains(&"Discharging") {
        ChargingState::Discharging
    } else if statuses.contains(&"Not charging") {
        ChargingState::NotCharging
    } else {
        ChargingState::Unknown
    };
    let power_source = if ac_online {
        PowerSource::Ac
    } else if level.is_some() {
        PowerSource::Battery
    } else {
        PowerSource::Unknown
    };
    BatteryState {
        level,
        charging,
        power_source,
        power_saver: false,
        thermal_state: ThermalState::Nominal,
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{BatteryState, aggregate_cells};

    const SYSFS_ROOT: &str = "/sys/class/power_supply";

    pub(super) fn sample() -> Option<BatteryState> {
        let (batteries, ac) = scan_devices();
        if batteries.is_empty() {
            return None;
        }
        let capacities: Vec<u8> = batteries
            .iter()
            .filter_map(|p| read_u64(&p.join("capacity")))
            .map(|v| u8::try_from(v.min(100)).unwrap_or(100))
            .collect();
        let statuses: Vec<String> = batteries
            .iter()
            .filter_map(|p| read_string(&p.join("status")))
            .collect();
        let statuses: Vec<&str> = statuses.iter().map(String::as_str).collect();
        let ac_online = ac
            .as_ref()
            .and_then(|p| read_u64(&p.join("online")))
            .is_some_and(|v| v == 1);
        Some(aggregate_cells(&capacities, &statuses, ac_online))
    }

    fn scan_devices() -> (Vec<PathBuf>, Option<PathBuf>) {
        let mut batteries: Vec<PathBuf> = Vec::new();
        let mut ac: Option<PathBuf> = None;
        let Ok(entries) = fs::read_dir(SYSFS_ROOT) else {
            return (batteries, ac);
        };
        // Collect + sort for stable iteration order across kernel
        // versions (`read_dir` is filesystem-order on Linux).
        let mut paths: Vec<PathBuf> = entries.flatten().map(|e| e.path()).collect();
        paths.sort();
        for path in paths {
            let kind = read_string(&path.join("type")).unwrap_or_default();
            match kind.as_str() {
                "Battery" => batteries.push(path),
                "Mains" | "USB" | "USB_PD" | "USB_C" | "Wireless" if ac.is_none() => {
                    ac = Some(path);
                }
                _ => {}
            }
        }
        (batteries, ac)
    }

    fn read_string(path: &Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
    }

    fn read_u64(path: &Path) -> Option<u64> {
        read_string(path).and_then(|s| s.parse().ok())
    }
}

/// `SYSTEM_POWER_STATUS` fields, as `GetSystemPowerStatus` fills them.
#[cfg(any(target_os = "windows", test))]
#[derive(Debug, Clone, Copy)]
struct PowerStatus {
    /// 0 offline, 1 online, 255 unknown.
    ac_line_status: u8,
    /// Bit set: 1 high, 2 low, 4 critical, 8 charging, 128 no system
    /// battery; the whole byte is 255 when the status is unknown.
    battery_flag: u8,
    /// 0..=100, or 255 when unknown.
    battery_life_percent: u8,
    /// 1 while battery saver is on.
    system_status_flag: u8,
}

#[cfg(any(target_os = "windows", test))]
impl PowerStatus {
    const FLAG_CHARGING: u8 = 8;
    const FLAG_NO_BATTERY: u8 = 128;
    const UNKNOWN: u8 = 255;

    fn to_battery_state(self) -> BatteryState {
        let level = match self.battery_life_percent {
            Self::UNKNOWN => None,
            n => Some(f32::from(n.min(100)) / 100.0),
        };
        let ac_online = match self.ac_line_status {
            0 => Some(false),
            1 => Some(true),
            _ => None,
        };
        // 255 is "unknown", not a bit set: it must be ruled out before
        // testing individual bits, or it would read as charging.
        let charging = match self.battery_flag {
            Self::UNKNOWN => ChargingState::Unknown,
            f if f & Self::FLAG_NO_BATTERY != 0 => ChargingState::Unknown,
            f if f & Self::FLAG_CHARGING != 0 => ChargingState::Charging,
            _ => match ac_online {
                Some(true) if level == Some(1.0) => ChargingState::Full,
                Some(true) => ChargingState::NotCharging,
                Some(false) => ChargingState::Discharging,
                None => ChargingState::Unknown,
            },
        };
        let has_battery =
            self.battery_flag != Self::UNKNOWN && self.battery_flag & Self::FLAG_NO_BATTERY == 0;
        let power_source = match ac_online {
            Some(true) => PowerSource::Ac,
            Some(false) => PowerSource::Battery,
            None => PowerSource::Unknown,
        };
        BatteryState {
            level: level.filter(|_| has_battery),
            charging,
            power_source,
            power_saver: self.system_status_flag & 1 != 0,
            thermal_state: ThermalState::Nominal,
        }
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    use super::{BatteryState, PowerStatus};

    pub(super) fn sample() -> Option<BatteryState> {
        // SAFETY: `SYSTEM_POWER_STATUS` is plain integer fields; all-zero
        // is a valid value.
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: fills the stack-allocated struct; returns non-zero on
        // success.
        let ok = unsafe { GetSystemPowerStatus(&raw mut status) };
        if ok == 0 {
            return None;
        }
        Some(
            PowerStatus {
                ac_line_status: status.ACLineStatus,
                battery_flag: status.BatteryFlag,
                battery_life_percent: status.BatteryLifePercent,
                system_status_flag: status.SystemStatusFlag,
            }
            .to_battery_state(),
        )
    }
}

/// One `IOPSGetPowerSourceDescription` entry of an internal battery.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, Default)]
struct IopsBattery {
    /// `Current Capacity`, a percentage on every Mac.
    current: Option<i64>,
    /// `Max Capacity`, `100` on every Mac.
    max: Option<i64>,
    /// `Is Charging`.
    charging: Option<bool>,
    /// `Is Charged`, only present while full on AC.
    charged: Option<bool>,
    /// `Power Source State` is `AC Power`.
    on_ac: Option<bool>,
}

/// `IOPSGetProvidingPowerSourceType`: what powers the machine now.
#[cfg(any(target_os = "macos", test))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ProvidingSource {
    Ac,
    Battery,
    Ups,
    Unknown,
}

/// Fold the `IOKit` readings into one snapshot. A machine without an
/// internal battery (Mac mini, iMac, Mac Pro) reports no level.
#[cfg(any(target_os = "macos", test))]
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    reason = "capacities are percentages and a ratio in 0..=1 fits f32"
)]
fn iops_state(
    batteries: &[IopsBattery],
    providing: ProvidingSource,
    power_saver: bool,
    thermal_state: isize,
) -> BatteryState {
    let levels: Vec<f32> = batteries
        .iter()
        .filter_map(|battery| {
            let (current, max) = (battery.current?, battery.max?);
            (max > 0).then(|| (current.clamp(0, max) as f64 / max as f64) as f32)
        })
        .collect();
    let level = (!levels.is_empty()).then(|| levels.iter().sum::<f32>() / levels.len() as f32);
    let on_ac = match providing {
        ProvidingSource::Ac => Some(true),
        ProvidingSource::Battery | ProvidingSource::Ups => Some(false),
        ProvidingSource::Unknown => batteries.iter().find_map(|battery| battery.on_ac),
    };
    let charging = if batteries.is_empty() {
        ChargingState::Unknown
    } else if batteries
        .iter()
        .any(|battery| battery.charging == Some(true))
    {
        ChargingState::Charging
    } else if batteries
        .iter()
        .all(|battery| battery.charged == Some(true))
    {
        ChargingState::Full
    } else {
        match on_ac {
            Some(true) => ChargingState::NotCharging,
            Some(false) => ChargingState::Discharging,
            None => ChargingState::Unknown,
        }
    };
    BatteryState {
        level,
        charging,
        power_source: match on_ac {
            Some(true) => PowerSource::Ac,
            Some(false) => PowerSource::Battery,
            None => PowerSource::Unknown,
        },
        power_saver,
        // `NSProcessInfoThermalState`.
        thermal_state: match thermal_state {
            ..=0 => ThermalState::Nominal,
            1 => ThermalState::Fair,
            2 => ThermalState::Serious,
            _ => ThermalState::Critical,
        },
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{msg_send, sel};

    use super::{BatteryState, IopsBattery, ProvidingSource, iops_state};
    use crate::macos_power::power_sources;

    pub(super) fn sample() -> Option<BatteryState> {
        let snapshot = power_sources()?;
        let batteries: Vec<IopsBattery> = snapshot
            .sources
            .iter()
            .filter(|source| source.kind == "InternalBattery" && source.present)
            .map(|source| IopsBattery {
                current: source.current,
                max: source.max,
                charging: source.charging,
                charged: source.charged,
                on_ac: source.on_ac,
            })
            .collect();
        let providing = match snapshot.providing.as_deref() {
            Some("AC Power") => ProvidingSource::Ac,
            Some("Battery Power") => ProvidingSource::Battery,
            Some("UPS Power") => ProvidingSource::Ups,
            _ => ProvidingSource::Unknown,
        };
        let (power_saver, thermal_state) = process_power_state();
        Some(iops_state(
            &batteries,
            providing,
            power_saver,
            thermal_state,
        ))
    }

    /// `NSProcessInfo` low power mode (macOS 12+) and thermal state.
    fn process_power_state() -> (bool, isize) {
        let Some(class) = AnyClass::get(c"NSProcessInfo") else {
            return (false, 0);
        };
        // SAFETY: `+processInfo` returns the shared instance; the
        // selectors below are NSProcessInfo API, `isLowPowerModeEnabled`
        // only sent after `respondsToSelector:` confirms it.
        unsafe {
            let info: *mut AnyObject = msg_send![class, processInfo];
            if info.is_null() {
                return (false, 0);
            }
            let thermal: isize = msg_send![info, thermalState];
            let has_low_power: bool =
                msg_send![info, respondsToSelector: sel!(isLowPowerModeEnabled)];
            let low_power: bool = has_low_power && msg_send![info, isLowPowerModeEnabled];
            (low_power, thermal)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_average_and_any_charging_cell_wins() {
        let state = aggregate_cells(&[80, 41], &["Discharging", "Charging"], true);
        assert_eq!(state.level, Some(0.605));
        assert_eq!(state.charging, ChargingState::Charging);
        assert_eq!(state.power_source, PowerSource::Ac);
    }

    #[test]
    fn cells_are_full_only_when_every_cell_is_full() {
        assert_eq!(
            aggregate_cells(&[100, 100], &["Full", "Full"], true).charging,
            ChargingState::Full
        );
        assert_eq!(
            aggregate_cells(&[100, 90], &["Full", "Not charging"], true).charging,
            ChargingState::NotCharging
        );
    }

    #[test]
    fn cells_without_readings_are_unknown() {
        let state = aggregate_cells(&[], &[], false);
        assert_eq!(state.level, None);
        assert_eq!(state.charging, ChargingState::Unknown);
        assert_eq!(state.power_source, PowerSource::Unknown);
    }

    fn status(ac: u8, flag: u8, percent: u8) -> BatteryState {
        PowerStatus {
            ac_line_status: ac,
            battery_flag: flag,
            battery_life_percent: percent,
            system_status_flag: 0,
        }
        .to_battery_state()
    }

    #[test]
    fn unknown_battery_flag_is_not_read_as_charging() {
        let state = status(1, 255, 255);
        assert_eq!(state.charging, ChargingState::Unknown);
        assert_eq!(state.level, None);
        assert_eq!(state.power_source, PowerSource::Ac);
    }

    #[test]
    fn no_system_battery_reports_no_level() {
        let state = status(1, 128, 100);
        assert_eq!(state.charging, ChargingState::Unknown);
        assert_eq!(state.level, None);
    }

    #[test]
    fn charging_bit_and_ac_line_map_to_charging_states() {
        assert_eq!(status(1, 8 | 1, 70).charging, ChargingState::Charging);
        assert_eq!(status(1, 1, 100).charging, ChargingState::Full);
        assert_eq!(status(1, 1, 90).charging, ChargingState::NotCharging);
        assert_eq!(status(0, 2, 20).charging, ChargingState::Discharging);
        assert_eq!(status(0, 2, 20).level, Some(0.2));
        assert_eq!(status(0, 2, 20).power_source, PowerSource::Battery);
    }

    #[test]
    fn battery_saver_flag_sets_power_saver() {
        let state = PowerStatus {
            ac_line_status: 0,
            battery_flag: 1,
            battery_life_percent: 80,
            system_status_flag: 1,
        }
        .to_battery_state();
        assert!(state.power_saver);
    }

    #[test]
    fn iops_laptop_on_battery() {
        let battery = IopsBattery {
            current: Some(42),
            max: Some(100),
            charging: Some(false),
            charged: None,
            on_ac: Some(false),
        };
        let state = iops_state(&[battery], ProvidingSource::Battery, true, 2);
        assert_eq!(state.level, Some(0.42));
        assert_eq!(state.charging, ChargingState::Discharging);
        assert_eq!(state.power_source, PowerSource::Battery);
        assert!(state.power_saver);
        assert_eq!(state.thermal_state, ThermalState::Serious);
    }

    #[test]
    fn iops_full_and_charging_on_ac() {
        let full = IopsBattery {
            current: Some(100),
            max: Some(100),
            charging: Some(false),
            charged: Some(true),
            on_ac: Some(true),
        };
        assert_eq!(
            iops_state(&[full], ProvidingSource::Ac, false, 0).charging,
            ChargingState::Full
        );
        let charging = IopsBattery {
            charging: Some(true),
            charged: None,
            ..full
        };
        assert_eq!(
            iops_state(&[charging], ProvidingSource::Ac, false, 0).charging,
            ChargingState::Charging
        );
        let held = IopsBattery {
            charged: None,
            ..full
        };
        assert_eq!(
            iops_state(&[held], ProvidingSource::Ac, false, 0).charging,
            ChargingState::NotCharging
        );
    }

    #[test]
    fn iops_desktop_mac_has_no_level() {
        let state = iops_state(&[], ProvidingSource::Ac, false, 7);
        assert_eq!(state.level, None);
        assert_eq!(state.charging, ChargingState::Unknown);
        assert_eq!(state.power_source, PowerSource::Ac);
        assert_eq!(state.thermal_state, ThermalState::Critical);
        assert_eq!(
            iops_state(&[], ProvidingSource::Ups, false, 0).power_source,
            PowerSource::Battery
        );
        let on_ac = IopsBattery {
            on_ac: Some(true),
            ..IopsBattery::default()
        };
        assert_eq!(
            iops_state(&[on_ac], ProvidingSource::Unknown, false, 0).power_source,
            PowerSource::Ac,
            "falls back to the battery's own power source state"
        );
    }
}
