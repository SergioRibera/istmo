//! Shared wire encoding: wincode laid out byte for byte like bincode 2's
//! `standard()` config (varint integers, little endian, `u32` enum tags
//! as varints, varint lengths).
//!
//! Decoding caps what a length prefix may make the decoder allocate. A
//! 9-byte input may claim a terabyte `Vec`, and wincode preallocates
//! up to its limit before reading the elements, so the limit follows the
//! input: 64 times its size, at least 64 KiB, never past [`MAX_DECODE`].
//! The limit is a const generic, so [`decode`] picks one of a few rungs.

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

/// Decoding rungs, smallest first; the last one is [`MAX_DECODE`].
pub type Rung16 = WireConfig<{ 1 << 16 }>;
/// See [`Rung16`].
pub type Rung20 = WireConfig<{ 1 << 20 }>;
/// See [`Rung16`].
pub type Rung24 = WireConfig<{ 1 << 24 }>;
/// See [`Rung16`].
pub type Rung28 = WireConfig<{ 1 << 28 }>;
/// See [`Rung16`].
pub type Rung30 = WireConfig<MAX_DECODE>;

/// Types that encode with the wire layout.
pub trait Encode: SchemaWrite<EncodeConfig, Src = Self> {}

impl<T> Encode for T where T: SchemaWrite<EncodeConfig, Src = T> {}

/// Types that decode with the wire layout under every rung.
pub trait Decode:
    SchemaReadOwned<Rung16, Dst = Self>
    + SchemaReadOwned<Rung20, Dst = Self>
    + SchemaReadOwned<Rung24, Dst = Self>
    + SchemaReadOwned<Rung28, Dst = Self>
    + SchemaReadOwned<Rung30, Dst = Self>
{
}

impl<T> Decode for T where
    T: SchemaReadOwned<Rung16, Dst = T>
        + SchemaReadOwned<Rung20, Dst = T>
        + SchemaReadOwned<Rung24, Dst = T>
        + SchemaReadOwned<Rung28, Dst = T>
        + SchemaReadOwned<Rung30, Dst = T>
{
}

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

/// [`decode`] with the allocation limit also capped at `cap`, rounded up
/// to the next rung.
///
/// # Errors
///
/// As [`decode`].
pub fn decode_capped<T: Decode>(bytes: &[u8], cap: usize) -> Result<(T, usize), CodecError> {
    let need = bytes.len().saturating_mul(64).max(1 << 16).min(cap);
    let mut rest = bytes;
    let value = if need <= 1 << 16 {
        <T as SchemaRead<'_, Rung16>>::get(&mut rest)
    } else if need <= 1 << 20 {
        <T as SchemaRead<'_, Rung20>>::get(&mut rest)
    } else if need <= 1 << 24 {
        <T as SchemaRead<'_, Rung24>>::get(&mut rest)
    } else if need <= 1 << 28 {
        <T as SchemaRead<'_, Rung28>>::get(&mut rest)
    } else {
        <T as SchemaRead<'_, Rung30>>::get(&mut rest)
    }?;
    Ok((value, bytes.len() - rest.len()))
}

/// [`decode`] that also refuses bytes left over after the value.
///
/// # Errors
///
/// As [`decode`], plus [`CodecError::TrailingBytes`].
pub fn decode_exact<T: Decode>(bytes: &[u8]) -> Result<T, CodecError> {
    let (value, read) = decode(bytes)?;
    if read == bytes.len() {
        Ok(value)
    } else {
        Err(CodecError::TrailingBytes {
            extra: bytes.len() - read,
        })
    }
}
