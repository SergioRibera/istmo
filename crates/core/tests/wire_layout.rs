//! The wire is wincode laid out like bincode 2's `standard()` config, which
//! the Kotlin and Swift codecs implement. Expected bytes were produced by
//! bincode 2.0.1; a wincode upgrade that changes any of them breaks the
//! native sides.

use std::collections::BTreeMap;
use std::fmt::Debug;
use std::time::Duration;

use istmo_core::CodecError;
use istmo_core::codec::{self, Decode, Encode};
use istmo_core::wincode::{SchemaRead, SchemaWrite};

// What `#[istmo::message]` expands to.
#[derive(Debug, PartialEq, SchemaWrite, SchemaRead)]
#[wincode(crate = "::istmo_core::wincode")]
enum Shape {
    Unit,
    Tuple(i32, String),
    Named { a: u8, b: Option<u64> },
}

// What `#[istmo::message]` expands to.
#[derive(Debug, PartialEq, SchemaWrite, SchemaRead)]
#[wincode(crate = "::istmo_core::wincode")]
struct Rec {
    id: [u8; 4],
    name: String,
    tags: Vec<String>,
    f: f32,
    d: f64,
    flag: bool,
    s: Shape,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[track_caller]
fn layout<T: Encode + Decode + PartialEq + Debug>(value: &T, expected: &str) {
    let bytes = codec::encode(value).expect("encode");
    assert_eq!(hex(&bytes), expected, "layout of {value:?}");
    let (back, read) = codec::decode::<T>(&bytes).expect("decode");
    assert_eq!(&back, value);
    assert_eq!(read, bytes.len());
}

#[test]
fn integers_match_bincode_varints() {
    layout(&255_u8, "ff");
    layout(&250_u16, "fa");
    layout(&251_u16, "fbfb00");
    layout(&u16::MAX, "fbffff");
    layout(&65_536_u32, "fc00000100");
    layout(&(1_u64 << 32), "fd0000000001000000");
    layout(&u64::MAX, "fdffffffffffffffff");
    layout(&7_u128, "07");
    layout(&(1_u128 << 64), "fe00000000000000000100000000000000");
    layout(&u128::MAX, "feffffffffffffffffffffffffffffffff");
    layout(&-5_i8, "fb");
    layout(&-300_i32, "fb5702");
    layout(&i64::MIN, "fdffffffffffffffff");
    layout(&(-1_i128 << 70), "feffffffffffffffff7f00000000000000");
    layout(&300_usize, "fb2c01");
    layout(&-2_isize, "03");
}

#[test]
fn scalars_and_text_match_bincode() {
    layout(&1.5_f32, "0000c03f");
    layout(&-0.25_f64, "000000000000d0bf");
    layout(&true, "01");
    layout(&'a', "61");
    layout(&'ñ', "c3b1");
    layout(&'€', "e282ac");
    layout(&'𝄞', "f09d849e");
    layout(&String::from("héllo"), "0668c3a96c6c6f");
    layout(&String::new(), "00");
}

#[test]
fn containers_match_bincode() {
    layout(&vec![1_u8, 2, 3], "03010203");
    layout(&None::<u32>, "00");
    layout(&Some(300_u32), "01fb2c01");
    layout(&(1_u8, String::from("x"), -1_i16), "01017801");
    layout(&(), "");
    layout(&[9_u16; 3], "090909");
    layout(
        &BTreeMap::from([(3_u8, String::from("c")), (1, String::from("a"))]),
        "02010161030163",
    );
    layout(&Duration::new(5, 7), "0507");
    layout(&Ok::<u8, String>(4), "0004");
    layout(&Err::<u8, String>("e".into()), "010165");
    layout(&vec![vec![1_u32, 70_000], vec![]], "020201fc7011010000");
}

#[test]
fn messages_match_bincode_derives() {
    layout(&Shape::Unit, "00");
    layout(&Shape::Tuple(-2, "t".into()), "01030174");
    layout(
        &Shape::Named {
            a: 1,
            b: Some(1 << 40),
        },
        "020101fd0000000000010000",
    );
    layout(
        &Rec {
            id: [1, 2, 3, 4],
            name: "n".into(),
            tags: vec!["a".into(), "bc".into()],
            f: 2.0,
            d: 3.5,
            flag: false,
            s: Shape::Named { a: 9, b: None },
        },
        "01020304016e020161026263000000400000000000000c4000020900",
    );
}

#[test]
fn a_claimed_length_past_the_input_is_refused() {
    // Nine bytes claiming a `Vec<u64>` of 2^56 elements.
    let mut bytes = vec![0xfd];
    bytes.extend_from_slice(&(1_u64 << 56).to_le_bytes());
    let err = codec::decode::<Vec<u64>>(&bytes).expect_err("must not decode");
    assert!(matches!(err, CodecError::Decode(_)), "{err}");
}

#[test]
fn the_limit_follows_the_input() {
    // A 4 MiB claim fits the bytes of a 4 MiB value but not of a tiny one.
    let big = vec![7_u8; 4 << 20];
    let bytes = codec::encode(&big).expect("encode");
    assert_eq!(codec::decode::<Vec<u8>>(&bytes).expect("decode").0, big);
    let err = codec::decode::<Vec<u8>>(&bytes[..16]).expect_err("must not decode");
    assert!(matches!(err, CodecError::Decode(_)), "{err}");
}

#[test]
fn a_cap_below_the_value_refuses_it() {
    let bytes = codec::encode(&vec![0_u8; 1 << 17]).expect("encode");
    let err = codec::decode_capped::<Vec<u8>>(&bytes, 1 << 16).expect_err("past the cap");
    assert!(matches!(err, CodecError::TooLong { .. }), "{err}");
    // Fewer bytes than the cap, claiming far past it.
    let mut claim = vec![0xfc];
    claim.extend_from_slice(&(1_u32 << 30).to_le_bytes());
    let err = codec::decode_capped::<Vec<u8>>(&claim, 1 << 16).expect_err("claim past the cap");
    assert!(matches!(err, CodecError::Decode(_)), "{err}");
}

#[test]
fn decode_exact_refuses_trailing_bytes() {
    let mut bytes = codec::encode(&5_u32).expect("encode");
    bytes.push(0);
    assert!(matches!(
        codec::decode_exact::<u32>(&bytes),
        Err(CodecError::TrailingBytes { extra: 1 })
    ));
}
