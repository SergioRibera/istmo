use istmo_core::{Runtime, codec};
use istmo_plugins::{
    ExternalMedium, ExternalStorage, STORAGE_INFO_CHANNEL, StorageInfo, StorageInfoClient,
};

fn sample() -> StorageInfo {
    StorageInfo {
        primary_free_bytes: 32 * 1024 * 1024 * 1024,
        primary_total_bytes: 128 * 1024 * 1024 * 1024,
        external: Some(ExternalStorage {
            free_bytes: 50 * 1024 * 1024 * 1024,
            total_bytes: 64 * 1024 * 1024 * 1024,
            medium: ExternalMedium::SdCard,
        }),
    }
}

fn publish(rt: &std::sync::Arc<Runtime>, info: &StorageInfo) {
    let bytes = codec::encode(info).expect("encode");
    rt.publish_early_latest(STORAGE_INFO_CHANNEL, bytes);
}

#[test]
fn current_is_none_before_publish() {
    let init = Runtime::mock();
    let plugin = StorageInfoClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current().unwrap().is_none());
}

#[test]
fn round_trip_with_external() {
    let init = Runtime::mock();
    let rt = init.runtime;
    publish(&rt, &sample());
    let plugin = StorageInfoClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current().unwrap(), Some(sample()));
}

#[test]
fn stream_delivers_low_storage_update() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = StorageInfoClient::from_runtime(&rt).expect("declared");
    let stream = plugin.updates();

    publish(&rt, &sample());
    assert_eq!(stream.recv().unwrap(), sample());

    let low = StorageInfo {
        primary_free_bytes: 128 * 1024 * 1024,
        primary_total_bytes: 128 * 1024 * 1024 * 1024,
        external: None,
    };
    publish(&rt, &low);
    assert_eq!(stream.recv().unwrap(), low);
}
