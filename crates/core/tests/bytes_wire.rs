//! Wire-layout parity between [`Bytes`] and `Vec<u8>`.
//!
//! The same message decoded as either type produces the same payload,
//! and encoding a `Bytes` field through the `FromInto<Vec<u8>, _>`
//! adapter produces bytes that round-trip to a `Vec<u8>` on the far
//! side.

use istmo_core::Bytes;
use istmo_core::codec;
use wincode::adapter::FromInto;
use wincode::{SchemaRead, SchemaWrite};

#[derive(SchemaRead, SchemaWrite, Debug, PartialEq, Eq)]
#[wincode(crate = "wincode")]
struct BytesPayload {
    #[wincode(with = "FromInto<Vec<u8>, _>")]
    body: Bytes,
}

#[derive(SchemaRead, SchemaWrite, Debug, PartialEq, Eq)]
#[wincode(crate = "wincode")]
struct VecPayload {
    body: Vec<u8>,
}

#[test]
fn bytes_encodes_like_vec_u8() {
    let body = vec![1u8, 2, 3, 4, 5, 9, 9, 9];

    let v = VecPayload { body: body.clone() };
    let b = BytesPayload {
        body: Bytes::from(body),
    };

    let v_bytes = codec::encode(&v).expect("encode vec");
    let b_bytes = codec::encode(&b).expect("encode bytes");
    assert_eq!(v_bytes, b_bytes, "wire layouts differ");
}

#[test]
fn bytes_decodes_from_vec_wire() {
    let v = VecPayload {
        body: vec![7u8, 7, 7, 7],
    };
    let wire = codec::encode(&v).expect("encode vec");

    let (b, _) = codec::decode::<BytesPayload>(&wire).expect("decode bytes");
    assert_eq!(b.body, v.body);
}

#[test]
fn vec_decodes_from_bytes_wire() {
    let b = BytesPayload {
        body: Bytes::from(vec![3u8, 1, 4, 1, 5, 9]),
    };
    let wire = codec::encode(&b).expect("encode bytes");

    let (v, _) = codec::decode::<VecPayload>(&wire).expect("decode vec");
    assert_eq!(v.body.as_slice(), b.body.as_slice());
}

#[test]
fn bytes_empty_roundtrips() {
    let b = BytesPayload { body: Bytes::new() };
    let wire = codec::encode(&b).expect("encode empty bytes");
    let (decoded, _) = codec::decode::<BytesPayload>(&wire).expect("decode empty");
    assert_eq!(decoded.body, Bytes::new());
    assert!(decoded.body.is_empty());
}
