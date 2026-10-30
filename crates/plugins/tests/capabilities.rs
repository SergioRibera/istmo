use istmo_core::{Runtime, codec};
use istmo_plugins::{CAPABILITIES_CHANNEL, Capabilities, CapabilitiesClient};

fn sample() -> Capabilities {
    Capabilities {
        has_biometric: true,
        has_face_id: true,
        has_fingerprint: false,
        has_nfc: true,
        has_haptics: true,
        has_camera_front: true,
        has_camera_back: true,
        has_gps: true,
        has_bluetooth: true,
        has_microphone: true,
        has_telephony: false,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, caps: &Capabilities) {
    let bytes = codec::encode(caps).expect("encode");
    rt.publish_early_latest(CAPABILITIES_CHANNEL, bytes);
}

#[test]
fn current_or_default_returns_zero_before_publish() {
    let init = Runtime::mock();
    let plugin = CapabilitiesClient::from_runtime(&init.runtime).expect("declared");
    assert_eq!(plugin.current_or_default().unwrap(), Capabilities::default());
}

#[test]
fn published_snapshot_overrides_default() {
    let init = Runtime::mock();
    let rt = init.runtime;
    publish(&rt, &sample());
    let plugin = CapabilitiesClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current_or_default().unwrap(), sample());
}

#[test]
fn updates_stream_delivers() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = CapabilitiesClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();
    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());
}
