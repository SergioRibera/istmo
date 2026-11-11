#![expect(unsafe_code, reason = "wincode cannot derive a recursive type")]
//! Wire impls for [`TypeRef`], laid out like the derive would (a `u32`
//! variant tag, then the variant's fields), recursing by hand.

use std::mem::MaybeUninit;

use wincode::config::Config;
use wincode::io::{Reader, Writer};
use wincode::{ReadError, ReadResult, SchemaRead, SchemaWrite, WriteResult};

use super::TypeRef;

/// Deepest `Vec`/`Option` nesting a decoded [`TypeRef`] may have.
const MAX_DEPTH: usize = 64;

impl TypeRef {
    const fn tag(&self) -> u32 {
        match self {
            Self::Unit => 0,
            Self::Bool => 1,
            Self::U8 => 2,
            Self::I8 => 3,
            Self::U16 => 4,
            Self::I16 => 5,
            Self::U32 => 6,
            Self::I32 => 7,
            Self::U64 => 8,
            Self::I64 => 9,
            Self::F32 => 10,
            Self::F64 => 11,
            Self::String => 12,
            Self::Bytes => 13,
            Self::Vec(_) => 14,
            Self::Option(_) => 15,
            Self::Named(_) => 16,
        }
    }

    fn wire_size<C: Config>(&self) -> WriteResult<usize> {
        let tag = <u32 as SchemaWrite<C>>::size_of(&self.tag())?;
        let body = match self {
            Self::Vec(inner) | Self::Option(inner) => inner.wire_size::<C>()?,
            Self::Named(name) => <String as SchemaWrite<C>>::size_of(name)?,
            _ => 0,
        };
        Ok(tag + body)
    }

    fn wire_write<C: Config>(&self, mut writer: impl Writer) -> WriteResult<()> {
        <u32 as SchemaWrite<C>>::write(writer.by_ref(), &self.tag())?;
        match self {
            Self::Vec(inner) | Self::Option(inner) => inner.wire_write::<C>(writer),
            Self::Named(name) => <String as SchemaWrite<C>>::write(writer, name),
            _ => Ok(()),
        }
    }

    fn wire_read<'de, C: Config>(mut reader: impl Reader<'de>, depth: usize) -> ReadResult<Self> {
        let tag = <u32 as SchemaRead<'de, C>>::get(reader.by_ref())?;
        let nested = |reader| {
            if depth >= MAX_DEPTH {
                return Err(ReadError::Custom("type nested too deep"));
            }
            Self::wire_read::<C>(reader, depth + 1).map(Box::new)
        };
        Ok(match tag {
            0 => Self::Unit,
            1 => Self::Bool,
            2 => Self::U8,
            3 => Self::I8,
            4 => Self::U16,
            5 => Self::I16,
            6 => Self::U32,
            7 => Self::I32,
            8 => Self::U64,
            9 => Self::I64,
            10 => Self::F32,
            11 => Self::F64,
            12 => Self::String,
            13 => Self::Bytes,
            14 => Self::Vec(nested(reader)?),
            15 => Self::Option(nested(reader)?),
            16 => Self::Named(<String as SchemaRead<'de, C>>::get(reader)?),
            other => return Err(ReadError::InvalidTagEncoding(other as usize)),
        })
    }
}

// SAFETY: `TYPE_META` stays `Dynamic`, and `size_of` walks the same
// variants `write` writes, so it returns exactly the bytes written.
unsafe impl<C: Config> SchemaWrite<C> for TypeRef {
    type Src = Self;

    fn size_of(src: &Self) -> WriteResult<usize> {
        src.wire_size::<C>()
    }

    fn write(writer: impl Writer, src: &Self) -> WriteResult<()> {
        src.wire_write::<C>(writer)
    }
}

// SAFETY: `TYPE_META` stays `Dynamic`, and `dst` is written only after
// the whole value decoded, right before returning `Ok`.
unsafe impl<'de, C: Config> SchemaRead<'de, C> for TypeRef {
    type Dst = Self;

    fn read(reader: impl Reader<'de>, dst: &mut MaybeUninit<Self>) -> ReadResult<()> {
        dst.write(Self::wire_read::<C>(reader, 0)?);
        Ok(())
    }
}
