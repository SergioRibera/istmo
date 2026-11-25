use wincode::{SchemaRead, SchemaWrite};

mod validate;
mod wire;

pub use validate::ContractError;

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct Contract {
    pub plugin_id: String,
    pub type_name: String,
    pub methods: Vec<Method>,
    pub init: Option<TypeRef>,
    pub types: Vec<TypeDef>,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum TypeDef {
    Struct(StructDef),
    Enum(EnumDef),
}

impl TypeDef {
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Struct(s) => &s.name,
            Self::Enum(e) => &e.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Vec<TypeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct Method {
    pub name: String,
    pub kind: MethodKind,
    pub args: Vec<Arg>,
    pub returns: TypeRef,
    pub error: Option<TypeRef>,
}

impl Method {
    /// Whether an argument is a frame sink: only a producer in the
    /// consumer's process can serve the method, so native clients never
    /// call it.
    #[must_use]
    pub fn takes_frame_sink(&self) -> bool {
        self.args.iter().any(|arg| arg.ty.is_frame_sink())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub enum MethodKind {
    Unary,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, SchemaWrite, SchemaRead)]
pub struct Arg {
    pub name: String,
    pub ty: TypeRef,
}

/// Wire impls are hand-written in the private `wire` module:
/// wincode cannot derive a recursive type.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum TypeRef {
    Unit,
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
    Vec(Box<Self>),
    Option(Box<Self>),
    Named(String),
    /// A `FrameSinkId<M>` argument: a `u64` on the wire, a typed sink
    /// (`IstmoFrameSender<M>`) on the Kotlin / Swift side, `M` being the
    /// metadata each frame carries. Only valid as a method argument.
    FrameSink(Box<Self>),
}

impl TypeRef {
    /// The Rust type name with module paths dropped (`Vec<Meta>`, `u32`):
    /// what `istmo_core::native_frame::meta_fingerprint` hashes.
    #[must_use]
    pub fn rust_short_name(&self) -> String {
        match self {
            Self::Unit => "()".to_owned(),
            Self::Bool => "bool".to_owned(),
            Self::U8 => "u8".to_owned(),
            Self::I8 => "i8".to_owned(),
            Self::U16 => "u16".to_owned(),
            Self::I16 => "i16".to_owned(),
            Self::U32 => "u32".to_owned(),
            Self::I32 => "i32".to_owned(),
            Self::U64 => "u64".to_owned(),
            Self::I64 => "i64".to_owned(),
            Self::F32 => "f32".to_owned(),
            Self::F64 => "f64".to_owned(),
            Self::String => "String".to_owned(),
            Self::Bytes => "Vec<u8>".to_owned(),
            Self::Vec(inner) => format!("Vec<{}>", inner.rust_short_name()),
            Self::Option(inner) => format!("Option<{}>", inner.rust_short_name()),
            Self::Named(name) => name.clone(),
            Self::FrameSink(meta) => format!("FrameSinkId<{}>", meta.rust_short_name()),
        }
    }

    /// The metadata fingerprint a native sender of `FrameSinkId<self>`
    /// presents: FNV-1a 64 of [`rust_short_name`](Self::rust_short_name),
    /// as a signed 64-bit value (Kotlin `Long` bits).
    #[must_use]
    pub fn meta_fingerprint(&self) -> u64 {
        self.rust_short_name()
            .bytes()
            .fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
                (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
            })
    }

    /// Whether this is a frame sink argument (`FrameSinkId<M>` or an
    /// `Option` of one).
    #[must_use]
    pub fn is_frame_sink(&self) -> bool {
        match self {
            Self::FrameSink(_) => true,
            Self::Option(inner) => matches!(**inner, Self::FrameSink(_)),
            _ => false,
        }
    }

    #[must_use]
    pub fn to_kotlin(&self) -> String {
        match self {
            Self::Unit => "Unit".to_owned(),
            Self::Bool => "Boolean".to_owned(),
            Self::U8 => "UByte".to_owned(),
            Self::I8 => "Byte".to_owned(),
            Self::U16 => "UShort".to_owned(),
            Self::I16 => "Short".to_owned(),
            Self::U32 => "UInt".to_owned(),
            Self::I32 => "Int".to_owned(),
            Self::U64 => "ULong".to_owned(),
            Self::I64 => "Long".to_owned(),
            Self::F32 => "Float".to_owned(),
            Self::F64 => "Double".to_owned(),
            Self::String => "String".to_owned(),
            Self::Bytes => "ByteArray".to_owned(),
            Self::Vec(inner) => format!("List<{}>", inner.to_kotlin()),
            Self::Option(inner) => format!("{}?", inner.to_kotlin()),
            Self::Named(name) => name.clone(),
            Self::FrameSink(meta) => format!("IstmoFrameSender<{}>", meta.to_kotlin()),
        }
    }

    #[must_use]
    pub fn to_swift(&self) -> String {
        match self {
            Self::Unit => "Void".to_owned(),
            Self::Bool => "Bool".to_owned(),
            Self::U8 => "UInt8".to_owned(),
            Self::I8 => "Int8".to_owned(),
            Self::U16 => "UInt16".to_owned(),
            Self::I16 => "Int16".to_owned(),
            Self::U32 => "UInt32".to_owned(),
            Self::I32 => "Int32".to_owned(),
            Self::U64 => "UInt64".to_owned(),
            Self::I64 => "Int64".to_owned(),
            Self::F32 => "Float".to_owned(),
            Self::F64 => "Double".to_owned(),
            Self::String => "String".to_owned(),
            Self::Bytes => "Data".to_owned(),
            Self::Vec(inner) => format!("[{}]", inner.to_swift()),
            Self::Option(inner) => format!("{}?", inner.to_swift()),
            Self::Named(name) => name.clone(),
            Self::FrameSink(meta) => format!("IstmoFrameSender<{}>", meta.to_swift()),
        }
    }
}
