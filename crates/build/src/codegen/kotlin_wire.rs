//! Kotlin read/write expressions over `dev.istmo.runtime.Bincode`.

use std::fmt::Write as _;

use super::{NamedAccess, Scalar, Shape};
use crate::contract::TypeRef;

/// Builds Kotlin expressions that decode / encode a [`TypeRef`].
///
/// Readers evaluate to `Bincode.Decoded<T>`; writers are `Unit`
/// expressions. Nested containers get lambda parameters indexed by depth
/// (`b1, c1`, `b2, c2`, …) so an inner lambda never reads an outer one's
/// buffer or cursor.
#[derive(Debug, Clone, Copy)]
pub(crate) struct KotlinWire {
    named: NamedAccess,
}

impl KotlinWire {
    pub(crate) const fn new(named: NamedAccess) -> Self {
        Self { named }
    }

    const fn prefix(self) -> &'static str {
        match self.named {
            NamedAccess::Codecs => "codecs.",
            NamedAccess::SelfImpl => "",
        }
    }

    /// `Bincode` reader for a scalar.
    pub(crate) const fn reader(scalar: Scalar) -> &'static str {
        match scalar {
            Scalar::Bool => "readBool",
            Scalar::U8 => "readU8",
            Scalar::I8 => "readI8",
            Scalar::U16 => "readU16",
            Scalar::I16 => "readI16",
            Scalar::U32 => "readU32",
            Scalar::I32 => "readI32",
            Scalar::U64 => "readU64",
            Scalar::I64 => "readI64",
            Scalar::F32 => "readF32",
            Scalar::F64 => "readF64",
            Scalar::String => "readString",
            Scalar::Bytes => "readBytes",
        }
    }

    /// `Bincode` writer for a scalar.
    pub(crate) const fn writer(scalar: Scalar) -> &'static str {
        match scalar {
            Scalar::Bool => "writeBool",
            Scalar::U8 => "writeU8",
            Scalar::I8 => "writeI8",
            Scalar::U16 => "writeU16",
            Scalar::I16 => "writeI16",
            Scalar::U32 => "writeU32",
            Scalar::I32 => "writeI32",
            Scalar::U64 => "writeU64",
            Scalar::I64 => "writeI64",
            Scalar::F32 => "writeF32",
            Scalar::F64 => "writeF64",
            Scalar::String => "writeString",
            Scalar::Bytes => "writeBytes",
        }
    }

    /// Expression decoding `ty` from `bytes` at `cursor`.
    pub(crate) fn read(self, ty: &TypeRef, bytes: &str, cursor: &str) -> String {
        self.read_at(ty, bytes, cursor, 1)
    }

    fn read_at(self, ty: &TypeRef, bytes: &str, cursor: &str, depth: usize) -> String {
        match ty.shape() {
            Shape::Scalar(s) => format!("Bincode.{}({bytes}, {cursor})", Self::reader(s)),
            Shape::Unit => format!("Bincode.Decoded(Unit, {cursor})"),
            Shape::Named(name) => format!("{}read{name}({bytes}, {cursor})", self.prefix()),
            Shape::Vec(inner) => self.read_container("readVec", inner, bytes, cursor, depth),
            Shape::Option(inner) => self.read_container("readOption", inner, bytes, cursor, depth),
        }
    }

    fn read_container(
        self,
        helper: &str,
        inner: &TypeRef,
        bytes: &str,
        cursor: &str,
        depth: usize,
    ) -> String {
        let (b, c) = (format!("b{depth}"), format!("c{depth}"));
        let body = self.read_at(inner, &b, &c, depth + 1);
        let b = if inner.is_unit() { "_" } else { b.as_str() };
        format!("Bincode.{helper}({bytes}, {cursor}) {{ {b}, {c} -> {body} }}")
    }

    /// Expression writing `value` (of type `ty`) into `buf`.
    pub(crate) fn write(self, ty: &TypeRef, buf: &str, value: &str) -> String {
        self.write_at(ty, buf, value, 1)
    }

    fn write_at(self, ty: &TypeRef, buf: &str, value: &str, depth: usize) -> String {
        match ty.shape() {
            Shape::Scalar(s) => format!("Bincode.{}({buf}, {value})", Self::writer(s)),
            Shape::Unit => "Unit".to_owned(),
            Shape::Named(name) => format!("{}write{name}({buf}, {value})", self.prefix()),
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
            return format!("Bincode.{helper}({buf}, {value}) {{ _, _ -> }}");
        }
        let (s, v) = (format!("s{depth}"), format!("v{depth}"));
        let body = self.write_at(inner, &s, &v, depth + 1);
        format!("Bincode.{helper}({buf}, {value}) {{ {s}, {v} -> {body} }}")
    }

    /// Emits a statement writing `value` into `buf`.
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

    /// Emits `val <local>` holding `ty` decoded from `bytes` at the
    /// `cursor` variable, and advances `cursor`.
    pub(crate) fn read_into(
        self,
        out: &mut String,
        indent: &str,
        ty: &TypeRef,
        local: &str,
        bytes: &str,
    ) {
        let tmp = format!("d_{}", local.trim_matches('`'));
        let expr = self.read(ty, bytes, "cursor");
        let _ = writeln!(out, "{indent}val {tmp} = {expr}; cursor = {tmp}.consumed");
        let _ = writeln!(out, "{indent}val {local} = {tmp}.value");
    }
}
