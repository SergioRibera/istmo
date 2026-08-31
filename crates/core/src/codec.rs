//! Bincode-based wire codec.
//!
//! `bincode` 2 provides its own `Encode`/`Decode` derives that do not require
//! `serde`, keeping this foundational crate serde-free.

use bincode::config::{Configuration, standard};

use crate::error::CodecError;

/// Codec configuration used by the istmo wire protocol.
#[must_use]
pub const fn wire_config() -> Configuration {
    standard()
}

/// Encodes a value into a byte vector using the wire configuration.
pub fn encode<T>(value: &T) -> Result<Vec<u8>, CodecError>
where
    T: bincode::Encode,
{
    bincode::encode_to_vec(value, wire_config()).map_err(CodecError::from)
}

/// Decodes a value from a byte slice using the wire configuration.
///
/// Returns the decoded value together with the number of bytes consumed. Any
/// trailing bytes past the encoded value are left untouched for callers that
/// need to drive their own framing.
pub fn decode<T>(bytes: &[u8]) -> Result<(T, usize), CodecError>
where
    T: bincode::Decode<()>,
{
    bincode::decode_from_slice(bytes, wire_config()).map_err(CodecError::from)
}
