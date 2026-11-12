//! Pins the wire bytes of the `coverage` contract's generated Rust types.
//!
//! The Kotlin and Swift codecs generated for the same contract must decode
//! `golden/coverage_wire.hex.expected` value by value and re-encode it byte
//! for byte; this test keeps the fixture in step with what Rust sends.

use std::fmt::{Debug, Write as _};
use std::fs;
use std::path::PathBuf;

use istmo_core::codec::{self, Decode, Encode};

#[expect(
    unreachable_pub,
    reason = "the generated golden declares its types `pub`"
)]
mod coverage {
    use istmo_core::NativeHandleId;

    include!("golden/coverage_types.rs.expected");
}

use coverage::{Everything, FirstError, Mode, SecondError, Shape};
use istmo_core::NativeHandleId;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut out, b| {
        let _ = write!(out, "{b:02x}");
        out
    })
}

/// Encodes `value`, checks it decodes back whole, and appends a
/// `<name> <hex>` fixture line.
fn line<T: Encode + Decode + PartialEq + Debug>(out: &mut String, name: &str, value: &T) {
    let bytes = codec::encode(value).expect("encode");
    let (back, read) = codec::decode::<T>(&bytes).expect("decode");
    assert_eq!(&back, value, "{name} round trip");
    assert_eq!(read, bytes.len(), "{name} consumed");
    let _ = writeln!(out, "{name} {}", hex(&bytes));
}

/// Every integer at a varint boundary or its type's extremes, non-ASCII
/// text and each container shape populated, empty and absent.
fn extremes() -> Everything {
    Everything {
        unit: (),
        flag: true,
        byte: u8::MAX,
        signed_byte: i8::MIN,
        short: 251,
        signed_short: i16::MIN,
        word: u32::MAX,
        signed_word: i32::MIN,
        long: u64::MAX,
        signed_long: i64::MIN,
        single: -1.5,
        double: 1e300,
        text: "héllo €𝄞".to_owned(),
        blob: vec![0, 0xff, 7],
        nested: vec![
            Some(vec!["a".to_owned(), "bc".to_owned()]),
            None,
            Some(vec![]),
        ],
        maybe_blob: Some(vec![]),
        grid: vec![vec![0, 250, 251, 65_535, 65_536], vec![]],
        shapes: Some(vec![
            Shape::Default,
            Shape::Circle(-0.0),
            Shape::Path(vec![i32::MAX, -1, 0], Some("p".to_owned())),
            Shape::Path(vec![], None),
        ]),
        handle: NativeHandleId(u64::MAX),
        object: "o".to_owned(),
        default: false,
        r#where: Some(i64::MAX),
        cursor: 0,
        c: 250,
    }
}

/// Small values, absent options, and signed 64-bit values whose zigzag
/// form sets the top bit (a sign-extending decode gets them wrong).
fn small() -> Everything {
    Everything {
        unit: (),
        flag: false,
        byte: 0,
        signed_byte: -1,
        short: 250,
        signed_short: -126,
        word: 65_536,
        signed_word: -300,
        long: 1 << 32,
        signed_long: 1 << 62,
        single: 0.0,
        double: -0.25,
        text: String::new(),
        blob: vec![],
        nested: vec![],
        maybe_blob: None,
        grid: vec![],
        shapes: None,
        handle: NativeHandleId(0),
        object: String::new(),
        default: true,
        r#where: Some(-(1 << 62) - 1),
        cursor: 1,
        c: 2,
    }
}

#[test]
fn coverage_values_match_golden_bytes() {
    let mut out = String::new();
    line(&mut out, "Everything", &extremes());
    line(&mut out, "Everything", &small());
    line(&mut out, "Shape", &Shape::Path(vec![-2], None));
    line(&mut out, "Mode", &Mode::In);
    line(&mut out, "Mode", &Mode::Out);
    line(&mut out, "FirstError", &FirstError::Denied);
    line(&mut out, "FirstError", &FirstError::Failed("ñ".to_owned()));
    line(&mut out, "SecondError", &SecondError { code: 300 });

    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/golden/coverage_wire.hex.expected");
    if std::env::var("ISTMO_BUILD_UPDATE_GOLDEN").is_ok() {
        fs::write(&path, &out).expect("update golden");
        return;
    }
    let expected =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    assert_eq!(
        out, expected,
        "coverage wire bytes changed: the Kotlin/Swift codecs must be re-checked against them"
    );
}
