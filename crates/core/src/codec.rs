//! Shared wire encoding: wincode laid out byte for byte like bincode 2's
//! `standard()` config (varint integers, little endian, `u32` enum tags
//! as varints, varint lengths).
//!
//! Decoding caps what a length prefix may make the decoder allocate. A
//! 9-byte input may claim a terabyte `Vec`, and wincode preallocates
//! up to its limit before reading the elements, so the limit follows the
//! input: 64 times its size, at least 64 KiB, never past [`MAX_DECODE`].
//! The limit is a const generic, so [`decode`] picks one of a few rungs,
//! each four times the last.

use wincode::config::{Configuration, PREALLOCATION_SIZE_LIMIT_DISABLED};
use wincode::int_encoding::{LittleEndian, VarInt};
use wincode::len::BincodeLen;
use wincode::{SchemaRead, SchemaReadOwned, SchemaWrite};

use crate::error::CodecError;

/// Wire layout with a preallocation limit of `LIMIT` bytes.
pub type WireConfig<const LIMIT: usize> =
    Configuration<true, LIMIT, BincodeLen, LittleEndian, VarInt, u32>;

/// Encoding config: our own values, so no preallocation limit.
pub type EncodeConfig = WireConfig<PREALLOCATION_SIZE_LIMIT_DISABLED>;

/// Largest allocation a decoded value may claim.
pub const MAX_DECODE: usize = 1 << 30;

/// Smallest limit [`decode`] works with when the input is tiny.
const MIN_DECODE: usize = 1 << 16;

/// Types that encode with the wire layout.
pub trait Encode: SchemaWrite<EncodeConfig, Src = Self> {}

impl<T: ?Sized + SchemaWrite<EncodeConfig, Src = T>> Encode for T {}

macro_rules! rungs {
    ($($shift:literal),+) => {
        /// Types that decode with the wire layout under every rung.
        pub trait Decode: $(SchemaReadOwned<WireConfig<{ 1 << $shift }>, Dst = Self> +)+ Sized {}

        impl<T> Decode for T where T: $(SchemaReadOwned<WireConfig<{ 1 << $shift }>, Dst = T> +)+ Sized {}

        /// Read a `T` from the front of `rest` with the smallest rung that
        /// holds `need`.
        fn read_within<T: Decode>(rest: &mut &[u8], need: usize) -> wincode::ReadResult<T> {
            $(
                if need <= 1 << $shift {
                    return <T as SchemaRead<'_, WireConfig<{ 1 << $shift }>>>::get(rest);
                }
            )+
            <T as SchemaRead<'_, WireConfig<MAX_DECODE>>>::get(rest)
        }
    };
}

rungs!(12, 14, 16, 18, 20, 22, 24, 26, 28, 30);

/// Encode `value` into a `Vec<u8>` with the wire layout.
///
/// # Errors
///
/// [`CodecError::Encode`] if wincode fails.
pub fn encode<T>(value: &T) -> Result<Vec<u8>, CodecError>
where
    T: Encode + ?Sized,
{
    wincode::config::serialize(value, EncodeConfig::new()).map_err(CodecError::from)
}

/// Decode a `T` from the front of `bytes`, returning the value and the
/// number of bytes consumed.
///
/// # Errors
///
/// [`CodecError::Decode`] if the bytes are not a valid `T`, or claim an
/// allocation past the limit derived from their length.
pub fn decode<T: Decode>(bytes: &[u8]) -> Result<(T, usize), CodecError> {
    decode_capped(bytes, MAX_DECODE)
}

/// [`decode`] for a value that may never take more than `cap` bytes:
/// longer input is refused outright, and no claim may pass `cap` rounded
/// up to the next rung (at most four times it).
///
/// # Errors
///
/// As [`decode`], plus [`CodecError::TooLong`].
pub fn decode_capped<T: Decode>(bytes: &[u8], cap: usize) -> Result<(T, usize), CodecError> {
    if bytes.len() > cap {
        return Err(CodecError::TooLong {
            len: bytes.len(),
            cap,
        });
    }
    let need = bytes.len().saturating_mul(64).max(MIN_DECODE).min(cap);
    let mut rest = bytes;
    let value = read_within(&mut rest, need)?;
    Ok((value, bytes.len() - rest.len()))
}

/// [`decode`] that also refuses bytes left over after the value.
///
/// # Errors
///
/// As [`decode`], plus [`CodecError::TrailingBytes`].
pub fn decode_exact<T: Decode>(bytes: &[u8]) -> Result<T, CodecError> {
    decode_exact_capped(bytes, MAX_DECODE)
}

/// [`decode_capped`] that also refuses bytes left over after the value.
///
/// # Errors
///
/// As [`decode_capped`], plus [`CodecError::TrailingBytes`].
pub fn decode_exact_capped<T: Decode>(bytes: &[u8], cap: usize) -> Result<T, CodecError> {
    let (value, read) = decode_capped(bytes, cap)?;
    if read == bytes.len() {
        Ok(value)
    } else {
        Err(CodecError::TrailingBytes {
            extra: bytes.len() - read,
        })
    }
}
