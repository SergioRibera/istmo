use bincode::{Decode, Encode};

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Contract {
    pub plugin_id: String,
    pub type_name: String,
    pub methods: Vec<Method>,
    pub init: Option<TypeRef>,
    pub types: Vec<TypeDef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
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

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct StructDef {
    pub name: String,
    pub fields: Vec<Field>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Field {
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct EnumDef {
    pub name: String,
    pub variants: Vec<EnumVariant>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct EnumVariant {
    pub name: String,
    pub payload: Vec<TypeRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Method {
    pub name: String,
    pub kind: MethodKind,
    pub args: Vec<Arg>,
    pub returns: TypeRef,
    pub error: Option<TypeRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub enum MethodKind {
    Unary,
    Stream,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Arg {
    pub name: String,
    pub ty: TypeRef,
}

#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
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

