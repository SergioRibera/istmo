use istmo_core::{Runtime, codec};
use istmo_plugins::{
    BATTERY_STATE_CHANNEL, BatteryState, BatteryStateClient, ChargingState, PowerSource,
    ThermalState,
};

fn sample() -> BatteryState {
    BatteryState {
        level: Some(0.72),
        charging: ChargingState::Discharging,
        power_source: PowerSource::Battery,
        power_saver: false,
        thermal_state: ThermalState::Nominal,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, state: &BatteryState) {
    let bytes = codec::encode(state).expect("encode");
    rt.publish_early_latest(BATTERY_STATE_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = BatteryStateClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn stream_delivers_charge_transition() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = BatteryStateClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();

    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());

    let charging = BatteryState {
        level: Some(0.73),
        charging: ChargingState::Charging,
        power_source: PowerSource::Ac,
        power_saver: false,
        thermal_state: ThermalState::Nominal,
    };
    publish(&rt, &charging);
    assert_eq!(stream.recv().unwrap(), charging);
}

#[test]
fn power_saver_and_thermal_round_trip() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let state = BatteryState {
        level: Some(0.08),
        charging: ChargingState::Discharging,
        power_source: PowerSource::Battery,
        power_saver: true,
        thermal_state: ThermalState::Serious,
    };
    publish(&rt, &state);
    let plugin = BatteryStateClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current().unwrap(), Some(state));
}
