//! Battery level, charging and thermal state stream.
//!
//! Published by native observers:
//! - Android: `Intent.ACTION_BATTERY_CHANGED` sticky receiver +
//!   `PowerManager.isPowerSaveMode` + `getCurrentThermalStatus`.
//! - Apple: `UIDevice.batteryLevel` / `batteryState` with
//!   `batteryMonitoringEnabled = true` (iOS); `IOPowerSources` on
//!   macOS; `ProcessInfo.isLowPowerModeEnabled` +
//!   `ProcessInfo.thermalState`.
//! - Linux: UPower D-Bus `org.freedesktop.UPower.Device`.
//! - Windows: `GetSystemPowerStatus`.
//! - Web: `navigator.getBattery()` (deprecated, best-effort).
//!
//! No special permissions are required on any platform.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const BATTERY_STATE_PLUGIN_ID: &str = "istmo.battery_state";
pub const BATTERY_STATE_CHANNEL: &str = "istmo.battery_state";

#[message(bincode = "::bincode")]
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

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ChargingState {
    #[default]
    Unknown,
    Charging,
    Discharging,
    NotCharging,
    Full,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PowerSource {
    #[default]
    Unknown,
    Battery,
    Ac,
    Usb,
    Wireless,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ThermalState {
    #[default]
    Nominal,
    Fair,
    Serious,
    Critical,
}

#[derive(Debug, Clone)]
pub struct BatteryStateClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for BatteryStateClient {
    const PLUGIN_ID: &'static str = BATTERY_STATE_PLUGIN_ID;
}

impl BatteryStateClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(BATTERY_STATE_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<BatteryState>, IstmoError> {
        self.slot.peek().map_or(Ok(None), |b| decode(&b).map(Some))
    }

    #[must_use]
    pub fn updates(&self) -> BatteryStateStream {
        BatteryStateStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct BatteryStateStream {
    rx: Receiver<Vec<u8>>,
}

impl BatteryStateStream {
    pub fn recv(&self) -> Result<BatteryState, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<BatteryState, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<BatteryState, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<BatteryState, IstmoError> {
    let (v, _) = codec::decode::<BatteryState>(bytes)?;
    Ok(v)
}
