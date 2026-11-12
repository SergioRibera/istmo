//! Swift read/write expressions over `IstmoRuntime.Bincode`.

use std::fmt::Write as _;

use super::{NamedAccess, Scalar, Shape};
use crate::contract::TypeRef;

/// Builds Swift expressions that decode / encode a [`TypeRef`].
///
/// Readers are `try` expressions over an `inout Bincode.Cursor`; writers
/// append to an `inout Data`. Nested containers get closure parameters
/// indexed by depth (`c1`, `o1, v1`, …).
#[derive(Debug, Clone, Copy)]
pub(crate) struct SwiftWire {
    named: NamedAccess,
}

impl SwiftWire {
    pub(crate) const fn new(named: NamedAccess) -> Self {
        Self { named }
    }

    const fn prefix(self) -> &'static str {
        match self.named {
            NamedAccess::Codecs => "codecs.",
            NamedAccess::SelfImpl => "self.",
        }
    }

    /// `Bincode` reader for a scalar. Readers narrower than 64 bits
    /// throw `DecodeError.outOfRange` instead of trapping.
    pub(crate) const fn reader(scalar: Scalar) -> &'static str {
        match scalar {
            Scalar::Bool => "readBool",
            Scalar::U8 => "readU8",
            Scalar::I8 => "readI8",
            Scalar::U16 => "readVarintU16",
            Scalar::I16 => "readVarintI16",
            Scalar::U32 => "readVarintU32",
            Scalar::I32 => "readVarintI32",
            Scalar::U64 => "readVarintU64",
            Scalar::I64 => "readVarintI64",
            Scalar::F32 => "readF32",
            Scalar::F64 => "readF64",
            Scalar::String => "readString",
            Scalar::Bytes => "readData",
        }
    }

    /// `Bincode` writer for a scalar.
    pub(crate) const fn writer(scalar: Scalar) -> &'static str {
        match scalar {
            Scalar::Bool => "writeBool",
            Scalar::U8 => "writeU8",
            Scalar::I8 => "writeI8",
            Scalar::U16 => "writeVarintU16",
            Scalar::I16 => "writeVarintI16",
            Scalar::U32 => "writeVarintU32",
            Scalar::I32 => "writeVarintI32",
            Scalar::U64 => "writeVarintU64",
            Scalar::I64 => "writeVarintI64",
            Scalar::F32 => "writeF32",
            Scalar::F64 => "writeF64",
            Scalar::String => "writeString",
            Scalar::Bytes => "writeData",
        }
    }

    /// Expression decoding `ty` from the `cursor` variable.
    pub(crate) fn read(self, ty: &TypeRef, cursor: &str) -> String {
        self.read_at(ty, cursor, 1)
    }

    fn read_at(self, ty: &TypeRef, cursor: &str, depth: usize) -> String {
        match ty.shape() {
            Shape::Scalar(s) => format!("try Bincode.{}(&{cursor})", Self::reader(s)),
            Shape::Unit => "()".to_owned(),
            Shape::Named(name) => format!("try {}read{name}(&{cursor})", self.prefix()),
            Shape::Vec(inner) => self.read_container("readVec", inner, cursor, depth),
            Shape::Option(inner) => self.read_container("readOption", inner, cursor, depth),
        }
    }

    fn read_container(self, helper: &str, inner: &TypeRef, cursor: &str, depth: usize) -> String {
        if inner.is_unit() {
            return format!("try Bincode.{helper}(&{cursor}) {{ _ in () }}");
        }
        let c = format!("c{depth}");
        let body = self.read_at(inner, &c, depth + 1);
        format!("try Bincode.{helper}(&{cursor}) {{ {c} in {body} }}")
    }

    /// Expression appending `value` (of type `ty`) to `buf`.
    pub(crate) fn write(self, ty: &TypeRef, buf: &str, value: &str) -> String {
        self.write_at(ty, buf, value, 1)
    }

    fn write_at(self, ty: &TypeRef, buf: &str, value: &str, depth: usize) -> String {
        match ty.shape() {
            Shape::Scalar(s) => format!("Bincode.{}(&{buf}, {value})", Self::writer(s)),
            Shape::Unit => "()".to_owned(),
            Shape::Named(name) => format!("{}write{name}(&{buf}, {value})", self.prefix()),
            Shape::Vec(inner) => self.write_container("writeVec", inner, buf, value, depth),
            Shape::Option(inner) => self.write_container("writeOption", inner, buf, value, depth),
        }
    }

    fn write_container(
        self,
        helper: &str,
        inner: &TypeRef,
        buf: &str,
        value: &str,
        depth: usize,
    ) -> String {
        if inner.is_unit() {
            return format!("Bincode.{helper}(&{buf}, {value}) {{ _, _ in }}");
        }
        let (o, v) = (format!("o{depth}"), format!("v{depth}"));
        let body = self.write_at(inner, &o, &v, depth + 1);
        format!("Bincode.{helper}(&{buf}, {value}) {{ {o}, {v} in {body} }}")
    }

    /// Emits a statement appending `value` to `buf`.
    pub(crate) fn write_stmt(
        self,
        out: &mut String,
        indent: &str,
        ty: &TypeRef,
        buf: &str,
        value: &str,
    ) {
        if ty.is_unit() {
            let _ = writeln!(out, "{indent}// () — nothing to encode");
        } else {
            let _ = writeln!(out, "{indent}{}", self.write(ty, buf, value));
        }
    }
}
