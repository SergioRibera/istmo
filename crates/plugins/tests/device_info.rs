use istmo_core::{Runtime, codec};
use istmo_plugins::{
    DEVICE_INFO_CHANNEL, DeviceInfo, DeviceInfoClient, FormFactor, ScreenInfo,
    device_info::rust_fallback,
};

fn sample() -> DeviceInfo {
    DeviceInfo {
        model: Some("Pixel 9".into()),
        manufacturer: Some("Google".into()),
        os_name: "android".into(),
        os_version: Some("15".into()),
        arch: "aarch64".into(),
        form_factor: FormFactor::Phone,
        cpu_cores: 8,
        ram_total_bytes: Some(12 * 1024 * 1024 * 1024),
        screen: Some(ScreenInfo {
            logical_width: 412,
            logical_height: 892,
            scale: 2.625,
            refresh_hz: Some(120.0),
        }),
        vendor_id: None,
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, snapshot: &DeviceInfo) {
    let bytes = codec::encode(snapshot).expect("encode");
    rt.publish_early_latest(DEVICE_INFO_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = DeviceInfoClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn current_or_fallback_returns_rust_defaults() {
    let init = Runtime::mock();
    let plugin = DeviceInfoClient::from_runtime(&init.runtime).expect("declared");
    let info = plugin.current_or_fallback().expect("fallback");
    assert_eq!(info.os_name, std::env::consts::OS);
    assert_eq!(info.arch, std::env::consts::ARCH);
    assert!(info.cpu_cores >= 1);
    assert!(info.model.is_none());
}

#[test]
fn published_snapshot_overrides_fallback() {
    let init = Runtime::mock();
    let rt = init.runtime;
    publish(&rt, &sample());
    let plugin = DeviceInfoClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current_or_fallback().unwrap(), sample());
}

#[test]
fn updates_stream_delivers() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = DeviceInfoClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();
    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());
}

#[test]
fn rust_fallback_populates_known_fields() {
    let info = rust_fallback();
    assert!(!info.os_name.is_empty());
    assert!(!info.arch.is_empty());
    assert!(info.cpu_cores >= 1);
}
