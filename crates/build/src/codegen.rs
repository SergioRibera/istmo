//! Building blocks shared by the Kotlin and Swift generators: the file
//! header, contract walks, identifier escaping and the per-type wire
//! read/write expressions. Every generator goes through these so the
//! encoding of a given [`TypeRef`] cannot drift between them.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use crate::contract::{Contract, Method, MethodKind, TypeDef, TypeRef};

mod ident;
mod kotlin_wire;
mod swift_wire;

pub(crate) use ident::{
    comment_text, fresh, kt_ident, kt_string, local, rust_ident, swift_ident, swift_label,
    swift_string,
};
pub(crate) use kotlin_wire::{KotlinWire, ReadSource};
pub(crate) use swift_wire::SwiftWire;

/// Runtime-provided named type every generator knows how to encode.
pub(crate) const NATIVE_HANDLE_ID: &str = "NativeHandleId";

/// How generated code reaches the reader / writer of a named type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NamedAccess {
    /// Through the injected `codecs` instance (dispatchers, clients).
    Codecs,
    /// On the enclosing codecs implementation itself.
    SelfImpl,
}

/// Fixed-layout wire values with a dedicated `Bincode` helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Scalar {
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F32,
    F64,
    String,
    Bytes,
}

/// A [`TypeRef`] split into what each generator handles separately.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Shape<'a> {
    Scalar(Scalar),
    Unit,
    Named(&'a str),
    Vec(&'a TypeRef),
    Option(&'a TypeRef),
}

impl TypeRef {
    pub(crate) const fn shape(&self) -> Shape<'_> {
        match self {
            Self::Unit => Shape::Unit,
            Self::Bool => Shape::Scalar(Scalar::Bool),
            Self::U8 => Shape::Scalar(Scalar::U8),
            Self::I8 => Shape::Scalar(Scalar::I8),
            Self::U16 => Shape::Scalar(Scalar::U16),
            Self::I16 => Shape::Scalar(Scalar::I16),
            Self::U32 => Shape::Scalar(Scalar::U32),
            Self::I32 => Shape::Scalar(Scalar::I32),
            Self::U64 => Shape::Scalar(Scalar::U64),
            Self::I64 => Shape::Scalar(Scalar::I64),
            Self::F32 => Shape::Scalar(Scalar::F32),
            Self::F64 => Shape::Scalar(Scalar::F64),
            Self::String => Shape::Scalar(Scalar::String),
            Self::Bytes => Shape::Scalar(Scalar::Bytes),
            Self::Vec(inner) => Shape::Vec(inner),
            Self::Option(inner) => Shape::Option(inner),
            Self::Named(name) => Shape::Named(name.as_str()),
        }
    }

    pub(crate) const fn is_unit(&self) -> bool {
        matches!(self, Self::Unit)
    }

    /// Calls `f` with every named type this reference mentions.
    pub(crate) fn for_each_named<'a>(&'a self, f: &mut impl FnMut(&'a str)) {
        match self {
            Self::Named(name) => f(name),
            Self::Vec(inner) | Self::Option(inner) => inner.for_each_named(f),
            Self::Unit
            | Self::Bool
            | Self::U8
            | Self::I8
            | Self::U16
            | Self::I16
            | Self::U32
            | Self::I32
            | Self::U64
            | Self::I64
            | Self::F32
            | Self::F64
            | Self::String
            | Self::Bytes => {}
        }
    }

    /// An upper-camel-case fragment naming this type inside a generated
    /// identifier (`Vec<String>` → `StringList`).
    pub(crate) fn ident_fragment(&self) -> String {
        match self {
            Self::Unit => "Unit".to_owned(),
            Self::Bool => "Bool".to_owned(),
            Self::U8 => "U8".to_owned(),
            Self::I8 => "I8".to_owned(),
            Self::U16 => "U16".to_owned(),
            Self::I16 => "I16".to_owned(),
            Self::U32 => "U32".to_owned(),
            Self::I32 => "I32".to_owned(),
            Self::U64 => "U64".to_owned(),
            Self::I64 => "I64".to_owned(),
            Self::F32 => "F32".to_owned(),
            Self::F64 => "F64".to_owned(),
            Self::String => "String".to_owned(),
            Self::Bytes => "Bytes".to_owned(),
            Self::Vec(inner) => format!("{}List", inner.ident_fragment()),
            Self::Option(inner) => format!("Optional{}", inner.ident_fragment()),
            Self::Named(name) => name.clone(),
        }
    }
}

impl Method {
    /// Whether an argument, the return or the error names a declared type,
    /// so encoding the call goes through `<T>Codecs`.
    pub(crate) fn signature_uses_named(&self) -> bool {
        let mut found = false;
        self.args
            .iter()
            .map(|a| &a.ty)
            .chain(std::iter::once(&self.returns))
            .chain(self.error.as_ref())
            .for_each(|ty| ty.for_each_named(&mut |_| found = true));
        found
    }
}

impl Contract {
    /// Every type reference in the method signatures and `init`.
    fn signature_types(&self) -> impl Iterator<Item = &TypeRef> {
        self.methods
            .iter()
            .flat_map(|m| {
                m.args
                    .iter()
                    .map(|a| &a.ty)
                    .chain(std::iter::once(&m.returns))
                    .chain(m.error.as_ref())
            })
            .chain(self.init.as_ref())
    }

    /// Every type reference inside the declared message types.
    fn member_types(&self) -> impl Iterator<Item = &TypeRef> {
        self.types
            .iter()
            .flat_map(|def| -> Box<dyn Iterator<Item = &TypeRef> + '_> {
                match def {
                    TypeDef::Struct(s) => Box::new(s.fields.iter().map(|f| &f.ty)),
                    TypeDef::Enum(e) => Box::new(e.variants.iter().flat_map(|v| v.payload.iter())),
                }
            })
    }

    /// Named types reachable from the signatures or the declared types.
    pub(crate) fn referenced_names(&self) -> BTreeSet<&str> {
        let mut set = BTreeSet::new();
        for ty in self.signature_types().chain(self.member_types()) {
            ty.for_each_named(&mut |name| {
                set.insert(name);
            });
        }
        set
    }

    /// Named types a `<T>Codecs` interface carries a reader / writer for:
    /// everything referenced plus every declared type, sorted.
    pub(crate) fn codec_type_names(&self) -> BTreeSet<&str> {
        let mut set = self.referenced_names();
        set.extend(self.types.iter().map(TypeDef::name));
        set
    }

    pub(crate) fn references_native_handle(&self) -> bool {
        self.referenced_names().contains(NATIVE_HANDLE_ID)
    }

    pub(crate) fn has_stream(&self) -> bool {
        self.methods
            .iter()
            .any(|m| matches!(m.kind, MethodKind::Stream))
    }

    /// Whether a stream declares a domain error its end can carry.
    pub(crate) fn has_failing_stream(&self) -> bool {
        self.methods
            .iter()
            .any(|m| matches!(m.kind, MethodKind::Stream) && m.error.is_some())
    }

    /// Whether a client must be handed a `<T>Codecs` instance.
    pub(crate) fn signatures_use_named(&self) -> bool {
        self.methods.iter().any(Method::signature_uses_named)
    }

    /// Distinct domain error types, in order of first use.
    pub(crate) fn error_types(&self) -> Vec<&TypeRef> {
        let mut out: Vec<&TypeRef> = Vec::new();
        for err in self.methods.iter().filter_map(|m| m.error.as_ref()) {
            if !out.contains(&err) {
                out.push(err);
            }
        }
        out
    }

    /// Client-side exception carrying `err`: `<T>Exception` while the
    /// contract has a single error type, `<T><Err>Exception` otherwise.
    pub(crate) fn exception_name(&self, err: &TypeRef) -> String {
        if self.error_types().len() <= 1 {
            format!("{}Exception", self.type_name)
        } else {
            format!("{}{}Exception", self.type_name, err.ident_fragment())
        }
    }
}

/// The two-line banner opening every generated file.
pub(crate) fn write_header(out: &mut String, contract: &Contract) {
    let _ = writeln!(out, "// GENERATED by istmo-build - DO NOT EDIT.");
    let _ = writeln!(out, "// plugin id: {}", comment_text(&contract.plugin_id));
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCALARS: [(TypeRef, Scalar); 13] = [
        (TypeRef::Bool, Scalar::Bool),
        (TypeRef::U8, Scalar::U8),
        (TypeRef::I8, Scalar::I8),
        (TypeRef::U16, Scalar::U16),
        (TypeRef::I16, Scalar::I16),
        (TypeRef::U32, Scalar::U32),
        (TypeRef::I32, Scalar::I32),
        (TypeRef::U64, Scalar::U64),
        (TypeRef::I64, Scalar::I64),
        (TypeRef::F32, Scalar::F32),
        (TypeRef::F64, Scalar::F64),
        (TypeRef::String, Scalar::String),
        (TypeRef::Bytes, Scalar::Bytes),
    ];

    /// How `crates/core/tests/wire_layout.rs` pins each scalar: `u8` / `i8`
    /// are one raw byte (`255u8` → `ff`, `-5i8` → `fb`), wider unsigned
    /// integers are varints, signed ones zigzag varints.
    const fn expected_helpers(
        s: Scalar,
    ) -> (&'static str, &'static str, &'static str, &'static str) {
        match s {
            Scalar::Bool => ("readBool", "writeBool", "readBool", "writeBool"),
            Scalar::U8 => ("readU8", "writeU8", "readU8", "writeU8"),
            Scalar::I8 => ("readI8", "writeI8", "readI8", "writeI8"),
            Scalar::U16 => ("readU16", "writeU16", "readVarintU16", "writeVarintU16"),
            Scalar::I16 => ("readI16", "writeI16", "readVarintI16", "writeVarintI16"),
            Scalar::U32 => ("readU32", "writeU32", "readVarintU32", "writeVarintU32"),
            Scalar::I32 => ("readI32", "writeI32", "readVarintI32", "writeVarintI32"),
            Scalar::U64 => ("readU64", "writeU64", "readVarintU64", "writeVarintU64"),
            Scalar::I64 => ("readI64", "writeI64", "readVarintI64", "writeVarintI64"),
            Scalar::F32 => ("readF32", "writeF32", "readF32", "writeF32"),
            Scalar::F64 => ("readF64", "writeF64", "readF64", "writeF64"),
            Scalar::String => ("readString", "writeString", "readString", "writeString"),
            Scalar::Bytes => ("readBytes", "writeBytes", "readData", "writeData"),
        }
    }

    #[test]
    fn scalar_helpers_match_the_pinned_wire_layout() {
        let kt = KotlinWire::new(NamedAccess::Codecs);
        let sw = SwiftWire::new(NamedAccess::Codecs);
        for (ty, scalar) in SCALARS {
            let (kr, kw, sr, swr) = expected_helpers(scalar);
            assert_eq!(kt.read(&ty, "b", "c"), format!("Bincode.{kr}(b, c)"));
            assert_eq!(kt.write(&ty, "o", "v"), format!("Bincode.{kw}(o, v)"));
            assert_eq!(sw.read(&ty, "c"), format!("try Bincode.{sr}(&c)"));
            assert_eq!(sw.write(&ty, "o", "v"), format!("Bincode.{swr}(&o, v)"));
        }
    }

    #[test]
    fn nested_containers_use_depth_indexed_parameters() {
        let ty = TypeRef::Vec(Box::new(TypeRef::Option(Box::new(TypeRef::Vec(Box::new(
            TypeRef::String,
        ))))));
        let kt = KotlinWire::new(NamedAccess::Codecs);
        assert_eq!(
            kt.read(&ty, "bytes", "cursor"),
            "Bincode.readVec(bytes, cursor) { b1, c1 -> Bincode.readOption(b1, c1) { b2, c2 -> \
             Bincode.readVec(b2, c2) { b3, c3 -> Bincode.readString(b3, c3) } } }"
        );
        assert_eq!(
            kt.write(&ty, "out", "value"),
            "Bincode.writeVec(out, value) { s1, v1 -> Bincode.writeOption(s1, v1) { s2, v2 -> \
             Bincode.writeVec(s2, v2) { s3, v3 -> Bincode.writeString(s3, v3) } } }"
        );
        let sw = SwiftWire::new(NamedAccess::SelfImpl);
        assert_eq!(
            sw.read(&ty, "c"),
            "try Bincode.readVec(&c) { c1 in try Bincode.readOption(&c1) { c2 in \
             try Bincode.readVec(&c2) { c3 in try Bincode.readString(&c3) } } }"
        );
        assert_eq!(
            sw.write(&ty, "out", "value"),
            "Bincode.writeVec(&out, value) { o1, v1 in Bincode.writeOption(&o1, v1) { o2, v2 in \
             Bincode.writeVec(&o2, v2) { o3, v3 in Bincode.writeString(&o3, v3) } } }"
        );
    }

    #[test]
    fn exception_names_split_per_error_type_only_when_needed() {
        use crate::contract::{Method, MethodKind};
        let method = |name: &str, err: &str| Method {
            name: name.to_owned(),
            kind: MethodKind::Unary,
            args: vec![],
            returns: TypeRef::Unit,
            error: Some(TypeRef::Named(err.to_owned())),
        };
        let mut contract = Contract {
            plugin_id: "x".to_owned(),
            type_name: "Foo".to_owned(),
            methods: vec![method("a", "AError"), method("b", "AError")],
            init: None,
            types: vec![],
        };
        let a = TypeRef::Named("AError".to_owned());
        assert_eq!(contract.exception_name(&a), "FooException");
        contract.methods.push(method("c", "BError"));
        assert_eq!(contract.exception_name(&a), "FooAErrorException");
    }
}
