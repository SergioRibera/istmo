use istmo_core::{Runtime, codec};
use istmo_plugins::{
    ChargingState, POWER_DEVICES_CHANNEL, PowerDevice, PowerDeviceKind, PowerDevices,
    PowerDevicesClient,
};

fn mouse() -> PowerDevice {
    PowerDevice {
        id: "a".to_owned(),
        name: None,
        kind: PowerDeviceKind::Mouse,
        level: Some(0.5),
        charging: ChargingState::Charging,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, devices: &PowerDevices) {
    let bytes = codec::encode(devices).expect("encode");
    rt.publish_early_latest(POWER_DEVICES_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = PowerDevicesClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn stream_delivers_device_changes() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let stream = PowerDevicesClient::from_runtime(&rt)
        .expect("declared")
        .stream();

    let one = PowerDevices {
        devices: vec![mouse()],
    };
    publish(&rt, &one);
    assert_eq!(stream.recv().unwrap(), one);

    let two = PowerDevices {
        devices: vec![
            mouse(),
            PowerDevice {
                id: "b".to_owned(),
                name: Some("Headphones".to_owned()),
                kind: PowerDeviceKind::Headphones,
                level: None,
                charging: ChargingState::Unknown,
            },
        ],
    };
    publish(&rt, &two);
    assert_eq!(stream.recv().unwrap(), two);
}

/// The Kotlin and Swift observers encode this layout by hand.
#[test]
fn wire_layout_is_pinned() {
    let devices = PowerDevices {
        devices: vec![mouse()],
    };
    assert_eq!(
        codec::encode(&devices).unwrap(),
        [
            0x01, // devices.len
            0x01, b'a', // id
            0x00, // name: None
            0x03, // kind: Mouse
            0x01, 0x00, 0x00, 0x00, 0x3F, // level: Some(0.5)
            0x01, // charging: Charging
        ]
    );
}
