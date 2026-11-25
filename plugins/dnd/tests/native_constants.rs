//! The native backends repeat the channel names, queue size, copy
//! directory and default limit: hold them to the Rust values.

const KOTLIN: &str = include_str!("../native/android/DragAndDropBackendImpl.kt");
const SWIFT: &str = include_str!("../native/ios/DragAndDropBackendImpl.swift");

#[test]
fn native_backends_use_the_rust_constants() {
    for (name, source) in [("kotlin", KOTLIN), ("swift", SWIFT)] {
        for value in [
            istmo_dnd::DROP_CHANNEL,
            istmo_dnd::DRAG_CHANNEL,
            istmo_dnd::DROP_DIR,
        ] {
            assert!(
                source.contains(&format!("\"{value}\"")),
                "{name} lacks {value}"
            );
        }
        assert!(
            source.contains(&format!("= {}", istmo_dnd::DROP_QUEUE_CAPACITY)),
            "{name} queue capacity"
        );
        assert_eq!(istmo_dnd::DEFAULT_MAX_BYTES, 512 * 1024 * 1024);
        assert!(source.contains("512"), "{name} default limit");
    }
    assert!(
        KOTLIN.contains(&format!("\"{}\"", istmo_dnd::DRAG_OUT_DIR)),
        "kotlin outgoing dir"
    );
}
