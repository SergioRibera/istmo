//! Pure-Rust desktop sampler for
//! [`super::battery_state::BatteryStateClient`].
//!
//! - Linux reads `/sys/class/power_supply/BAT*/` sysfs attributes.
//! - Windows calls `GetSystemPowerStatus` via `windows-sys`.
//! - macOS (IOPowerSources) and BSDs are stubs — plug the native
//!   bridge via `IstmoBatteryStateObserver` (iOS/macOS Swift) or
//!   publish bytes directly through `Runtime::publish_early_latest`.
//!
//! Call [`tick`] on an interval (every ~15s) or on power events the
//! surrounding app observes — the sampler publishes only when the
//! snapshot actually changes.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::battery_state::{
    BATTERY_STATE_CHANNEL, BATTERY_STATE_PLUGIN_ID, BatteryState, ChargingState, PowerSource,
    ThermalState,
};

#[derive(Debug)]
pub struct BatteryStatePublisher {
    runtime: Weak<Runtime>,
    last: Option<BatteryState>,
}

impl BatteryStatePublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(BATTERY_STATE_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last: None,
        })
    }

    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(sample) = sample() else {
            return;
        };
        if self.last.as_ref() == Some(&sample) {
            return;
        }
        match codec::encode(&sample) {
            Ok(bytes) => {
                runtime.publish_early_latest(BATTERY_STATE_CHANNEL, bytes);
                self.last = Some(sample);
            }
            Err(err) => tracing::warn!(target = "istmo.battery_state", ?err, "encode failed"),
        }
    }
}

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

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;
    use std::path::PathBuf;

    use super::{BatteryState, ChargingState, PowerSource, ThermalState};

    const SYSFS_ROOT: &str = "/sys/class/power_supply";

    pub(super) fn sample() -> Option<BatteryState> {
        let (batteries, ac) = scan_devices();
        if batteries.is_empty() {
            return None;
        }
        // Dual-battery laptops (e.g. ThinkPad BAT0/BAT1) expose one
        // sysfs entry per cell. Average the capacities (energy_now
        // isn't universally present) and prefer a "Charging" status
        // when any cell is charging.
        let caps: Vec<u8> = batteries
            .iter()
            .filter_map(|p| read_u64(&p.join("capacity")))
            .filter_map(|v| u8::try_from(v.min(100)).ok())
            .collect();
        let capacity = if caps.is_empty() {
            None
        } else {
            let sum: u32 = caps.iter().map(|v| u32::from(*v)).sum();
            Some(sum as f32 / caps.len() as f32 / 100.0)
        };
        let statuses: Vec<String> = batteries
            .iter()
            .filter_map(|p| read_string(&p.join("status")))
            .collect();
        let charging = if statuses.iter().any(|s| s.trim() == "Charging") {
            ChargingState::Charging
        } else if statuses.iter().all(|s| s.trim() == "Full") && !statuses.is_empty() {
            ChargingState::Full
        } else if statuses.iter().any(|s| s.trim() == "Discharging") {
            ChargingState::Discharging
        } else if statuses.iter().any(|s| s.trim() == "Not charging") {
            ChargingState::NotCharging
        } else {
            ChargingState::Unknown
        };
        let online = ac
            .as_ref()
            .and_then(|p| read_u64(&p.join("online")))
            .unwrap_or(0)
            == 1;
        let source = if online {
            PowerSource::Ac
        } else if capacity.is_some() {
            PowerSource::Battery
        } else {
            PowerSource::Unknown
        };
        Some(BatteryState {
            level: capacity,
            charging,
            power_source: source,
            power_saver: false,
            thermal_state: ThermalState::Nominal,
        })
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
            match kind.trim() {
                "Battery" => batteries.push(path),
                "Mains" | "USB" | "USB_PD" | "USB_C" | "Wireless" if ac.is_none() => {
                    ac = Some(path);
                }
                _ => {}
            }
        }
        (batteries, ac)
    }

    fn read_string(path: &std::path::Path) -> Option<String> {
        fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
    }

    fn read_u64(path: &std::path::Path) -> Option<u64> {
        read_string(path).and_then(|s| s.parse().ok())
    }
}

#[cfg(target_os = "windows")]
#[allow(unsafe_code)]
mod windows {
    use windows_sys::Win32::System::Power::{GetSystemPowerStatus, SYSTEM_POWER_STATUS};

    use super::{BatteryState, ChargingState, PowerSource, ThermalState};

    pub(super) fn sample() -> Option<BatteryState> {
        let mut status: SYSTEM_POWER_STATUS = unsafe { std::mem::zeroed() };
        // SAFETY: Fills a stack-allocated `SYSTEM_POWER_STATUS`; returns
        // non-zero on success.
        let ok = unsafe { GetSystemPowerStatus(&raw mut status) };
        if ok == 0 {
            return None;
        }
        let level = match status.BatteryLifePercent {
            255 => None,
            n => Some(f32::from(n) / 100.0),
        };
        let charging = match status.BatteryFlag {
            // High bit of BatteryFlag indicates charging.
            n if n & 8 != 0 => ChargingState::Charging,
            // BATTERY_FLAG_NO_SYSTEM_BATTERY
            128 => ChargingState::Unknown,
            _ => match status.ACLineStatus {
                1 => ChargingState::NotCharging,
                _ => ChargingState::Discharging,
            },
        };
        let source = match status.ACLineStatus {
            1 => PowerSource::Ac,
            0 => PowerSource::Battery,
            _ => PowerSource::Unknown,
        };
        let power_saver = status.SystemStatusFlag != 0;
        Some(BatteryState {
            level,
            charging,
            power_source: source,
            power_saver,
            thermal_state: ThermalState::Nominal,
        })
    }
}
