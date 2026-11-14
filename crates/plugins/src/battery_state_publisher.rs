//! Pure-Rust desktop sampler for
//! [`BatteryStateClient`](super::battery_state::BatteryStateClient).
//!
//! - Linux reads `/sys/class/power_supply/BAT*/` sysfs attributes.
//! - Windows calls `GetSystemPowerStatus` via `windows-sys`.
//! - macOS has no sampler: `tick` publishes nothing there. Publish the
//!   snapshot yourself through `Runtime::publish_early_latest` if you
//!   need it.
//!
//! Call [`BatteryStatePublisher::tick`] on an interval (every ~15s) or
//! on power events the surrounding app observes — the sampler publishes
//! only when the snapshot actually changes.

use super::battery_state::BatteryState;
#[cfg(any(target_os = "linux", target_os = "windows", test))]
use super::battery_state::{ChargingState, PowerSource, ThermalState};
use crate::early::LatestPublisher;

/// Publishes the host's battery snapshot.
pub type BatteryStatePublisher = LatestPublisher<BatteryState>;

impl LatestPublisher<BatteryState> {
    /// Sample the host and publish if the snapshot changed.
    pub fn tick(&mut self) {
        if let Some(sample) = sample() {
            self.publish_if_changed(sample);
        }
    }
}

#[cfg_attr(
    not(any(target_os = "linux", target_os = "windows")),
    expect(
        clippy::missing_const_for_fn,
        reason = "only the unsupported-target body is const"
    )
)]
fn sample() -> Option<BatteryState> {
    #[cfg(target_os = "linux")]
    {
        linux::sample()
    }
    #[cfg(target_os = "windows")]
    {
        windows::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
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
}
