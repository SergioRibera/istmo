//! The Android trampoline and the iOS helper encode `DeepLink` by hand
//! (`LauncherActionLink.{kt,swift}`). This pins the bytes Rust decodes;
//! `tests/native/check-wire.sh` runs the Kotlin and Swift encoders on the
//! same cases and compares their output with the same file.
//!
//! Refresh after an intended change with
//! `ISTMO_BUILD_UPDATE_GOLDEN=1 cargo test -p istmo-launcher-actions --test deep_link_wire`.

use std::fmt::Write as _;
use std::path::PathBuf;

use istmo_launcher_actions::{ACTION_SOURCE_PREFIX, action_source};
use istmo_plugins::{DEEPLINKS_CHANNEL, DEFAULT_DEEPLINKS_CAPACITY, DeepLink};

const KOTLIN: &str = include_str!("../native/android/LauncherActionLink.kt");
const SWIFT: &str = include_str!("../native/ios/LauncherActionLink.swift");
const ACTIVITY: &str = include_str!("../native/android/IstmoLauncherActionActivity.kt");

/// `(label, uri, id, received_at_ms)`; the native runners use the same.
const CASES: &[(&str, &str, Option<&str>, Option<u64>)] = &[
    (
        "full",
        "notes://compose",
        Some("compose"),
        Some(1_700_000_000_123),
    ),
    ("bare", "notes://x", None, None),
    (
        "small-ms",
        "com.example.notes:/a?b=c",
        Some("a.b-c_9"),
        Some(250),
    ),
    ("mid-ms", "notes://é", Some("x"), Some(65_536)),
];

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

fn rendered() -> String {
    let mut out = String::new();
    for &(label, uri, id, ms) in CASES {
        let link = DeepLink {
            uri: uri.into(),
            source: id.map(action_source),
            received_at_ms: ms,
        };
        let bytes = istmo_core::codec::encode(&link).expect("encode");
        let (back, _) = istmo_core::codec::decode::<DeepLink>(&bytes).expect("decode");
        assert_eq!(back, link);
        let _ = writeln!(out, "{label} {}", hex(&bytes));
    }
    out
}

#[test]
fn deep_link_bytes_are_pinned() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/deep_link.hex.expected");
    let actual = rendered();
    if std::env::var_os("ISTMO_BUILD_UPDATE_GOLDEN").is_some() {
        std::fs::write(&path, &actual).expect("update golden");
        return;
    }
    let expected = std::fs::read_to_string(&path).expect("read golden");
    assert_eq!(actual, expected);
}

#[test]
fn native_encoders_use_the_rust_constants() {
    for (name, source) in [("kotlin", KOTLIN), ("swift", SWIFT)] {
        for value in [DEEPLINKS_CHANNEL, ACTION_SOURCE_PREFIX] {
            assert!(
                source.contains(&format!("\"{value}\"")),
                "{name} lacks {value}"
            );
        }
        assert!(
            source.contains(&format!("= {DEFAULT_DEEPLINKS_CAPACITY}")),
            "{name} capacity"
        );
    }
    assert!(ACTIVITY.contains("\"dev.istmo.launcher_actions.ID\""));
}
