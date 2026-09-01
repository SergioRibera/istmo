//! Plugin contract data model shared by macros and code generators.

/// Complete description of a plugin trait, sufficient to generate the client
/// wrapper on the Rust side and the interface / protocol declarations on the
/// native side.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Contract {
    /// Dotted plugin identifier used on the wire, e.g. `"com.example.echo"`.
    pub plugin_id: String,
    /// Rust / Kotlin / Swift type name, e.g. `"Echo"`.
    pub type_name: String,
    /// Methods declared in the trait, in declaration order.
    pub methods: Vec<Method>,
    /// Type carried by `CreateInstance` when the plugin uses `acquire_with`.
    /// `None` for stateless plugins.
    pub init: Option<TypeRef>,
}

/// One method exposed by the plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Method {
    /// Method identifier as it appears in the trait.
    pub name: String,
    /// Whether the method is a unary call or a stream subscription.
    pub kind: MethodKind,
    /// Ordered list of arguments (excluding `&self`).
    pub args: Vec<Arg>,
    /// Success value returned by a unary call, or per-event value for a stream.
    pub returns: TypeRef,
    /// Optional domain error type. `Some` when the Rust signature is
    /// `Result<T, E>`; `None` when the method is infallible on the domain
    /// side (transport errors are always still possible).
    pub error: Option<TypeRef>,
}

/// Whether a method is unary (single response) or a stream (many events).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MethodKind {
    Unary,
    Stream,
}

/// A named method argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Arg {
    pub name: String,
    pub ty: TypeRef,
}

/// Type reference used on the wire.
///
/// Only the primitives listed here are understood by the built-in Kotlin /
/// Swift generators. User-defined types are carried through as [`Named`]
/// entries with the Rust type name; the generators emit that name verbatim
/// on the native side, so the plugin author is expected to declare a
/// matching Kotlin / Swift type (with matching wire representation) in the
/// generated module's companion source.
///
/// [`Named`]: TypeRef::Named
#[derive(Debug, Clone, PartialEq, Eq)]
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
}

impl TypeRef {
    /// Kotlin syntax for this type reference.
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
        }
    }

    /// Swift syntax for this type reference.
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
        }
    }
}
