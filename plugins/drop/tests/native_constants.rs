//! The native backends repeat the channel names, queue size, copy
//! directory and default limit: hold them to the Rust values.

const KOTLIN: &str = include_str!("../native/android/FileDropBackendImpl.kt");
const SWIFT: &str = include_str!("../native/ios/FileDropBackendImpl.swift");

#[test]
fn native_backends_use_the_rust_constants() {
    for (name, source) in [("kotlin", KOTLIN), ("swift", SWIFT)] {
        for value in [
            istmo_drop::DROP_CHANNEL,
            istmo_drop::DRAG_CHANNEL,
            istmo_drop::DROP_DIR,
        ] {
            assert!(
                source.contains(&format!("\"{value}\"")),
                "{name} lacks {value}"
            );
        }
        assert!(
            source.contains(&format!("= {}", istmo_drop::DROP_QUEUE_CAPACITY)),
            "{name} queue capacity"
        );
        assert_eq!(istmo_drop::DEFAULT_MAX_BYTES, 512 * 1024 * 1024);
        assert!(source.contains("512"), "{name} default limit");
    }
}
