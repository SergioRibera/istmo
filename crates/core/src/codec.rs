//! Shared bincode configuration used across the wire.

use bincode::config::{Configuration, standard};

use crate::error::CodecError;

/// The bincode configuration every istmo runtime uses.
///
/// Currently the default variable-int-encoded, little-endian `standard`
/// config. Pinning it here keeps every producer and consumer in the
/// framework byte-compatible.
#[must_use]
pub const fn wire_config() -> Configuration {
    standard()
}

/// Encode `value` into a `Vec<u8>` with the shared wire configuration.
///
/// # Errors
///
/// [`CodecError::Encode`] if bincode fails.
pub fn encode<T>(value: &T) -> Result<Vec<u8>, CodecError>
where
    T: bincode::Encode,
{
    bincode::encode_to_vec(value, wire_config()).map_err(CodecError::from)
}

/// Decode a `T` from bytes, returning the value and the number of
/// bytes consumed.
///
/// # Errors
///
/// [`CodecError::Decode`] if bincode fails.
pub fn decode<T>(bytes: &[u8]) -> Result<(T, usize), CodecError>
where
    T: bincode::Decode<()>,
{
    bincode::decode_from_slice(bytes, wire_config()).map_err(CodecError::from)
}
