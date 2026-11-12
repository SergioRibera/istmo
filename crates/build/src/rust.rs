use std::collections::BTreeMap;
use std::fmt::Write as _;

use crate::codegen::{NATIVE_HANDLE_ID, rust_ident, write_header};
use crate::contract::{Contract, EnumDef, StructDef, TypeDef, TypeRef};

#[must_use]
pub fn generate_rust_types(contract: &Contract) -> String {
    let derives = DeriveTable::new(&contract.types);
    let mut out = String::new();
    write_header(&mut out, contract);
    for ty in &contract.types {
        let _ = writeln!(out);
        let list = derives.of_def(ty).list();
        match ty {
            TypeDef::Struct(s) => write_struct(&mut out, s, &list),
            TypeDef::Enum(e) => write_enum(&mut out, e, &list),
        }
    }
    out
}

fn write_struct(out: &mut String, s: &StructDef, derives: &[&str]) {
    let _ = writeln!(out, "#[::istmo_macros::message(wincode = \"::wincode\")]");
    let _ = writeln!(out, "#[derive({})]", derives.join(", "));
    let _ = writeln!(out, "pub struct {} {{", s.name);
    for f in &s.fields {
        let _ = writeln!(
            out,
            "    pub {}: {},",
            rust_ident(&to_rust_field(&f.name)),
            rust_ty(&f.ty)
        );
    }
    let _ = writeln!(out, "}}");
}

fn write_enum(out: &mut String, e: &EnumDef, derives: &[&str]) {
    let _ = writeln!(out, "#[::istmo_macros::message(wincode = \"::wincode\")]");
    let _ = writeln!(out, "#[derive({})]", derives.join(", "));
    let _ = writeln!(out, "pub enum {} {{", e.name);
    for v in &e.variants {
        if v.payload.is_empty() {
            let _ = writeln!(out, "    {},", v.name);
        } else {
            let tys = v.payload.iter().map(rust_ty).collect::<Vec<_>>().join(", ");
            let _ = writeln!(out, "    {}({tys}),", v.name);
        }
    }
    let _ = writeln!(out, "}}");
}

/// Which of the optional derives a type supports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Derives {
    eq: bool,
    hash: bool,
    copy: bool,
}

impl Derives {
    const ALL: Self = Self {
        eq: true,
        hash: true,
        copy: true,
    };
    const NONE: Self = Self {
        eq: false,
        hash: false,
        copy: false,
    };

    const fn and(self, other: Self) -> Self {
        Self {
            eq: self.eq && other.eq,
            hash: self.hash && other.hash,
            copy: self.copy && other.copy,
        }
    }

    fn list(self) -> Vec<&'static str> {
        let mut out = vec!["Debug", "Clone", "PartialEq"];
        if self.eq {
            out.push("Eq");
        }
        if self.eq && self.hash {
            out.push("Hash");
        }
        if self.copy {
            out.push("Copy");
        }
        out
    }
}

/// [`Derives`] of every declared type, solved as a greatest fixed point:
/// each type starts with everything and loses what a member cannot
/// provide until nothing changes. Recursive types (through `Vec` /
/// `Option`) therefore terminate instead of recursing forever.
struct DeriveTable<'a> {
    derives: BTreeMap<&'a str, Derives>,
}

impl<'a> DeriveTable<'a> {
    fn new(all: &'a [TypeDef]) -> Self {
        let mut table = Self {
            derives: all.iter().map(|d| (d.name(), Derives::ALL)).collect(),
        };
        loop {
            let mut changed = false;
            for def in all {
                let next = table.of_def(def);
                if table.derives.insert(def.name(), next) != Some(next) {
                    changed = true;
                }
            }
            if !changed {
                return table;
            }
        }
    }

    fn of_def(&self, def: &TypeDef) -> Derives {
        match def {
            TypeDef::Struct(s) => s
                .fields
                .iter()
                .fold(Derives::ALL, |acc, f| acc.and(self.of_ref(&f.ty))),
            TypeDef::Enum(e) => {
                let members = e
                    .variants
                    .iter()
                    .flat_map(|v| &v.payload)
                    .fold(Derives::ALL, |acc, ty| acc.and(self.of_ref(ty)));
                // Only field-less enums are marked `Copy`.
                Derives {
                    copy: e.variants.iter().all(|v| v.payload.is_empty()),
                    ..members
                }
            }
        }
    }

    fn of_ref(&self, ty: &TypeRef) -> Derives {
        match ty {
            TypeRef::F32 | TypeRef::F64 => Derives {
                copy: true,
                ..Derives::NONE
            },
            TypeRef::Bytes => Derives {
                eq: true,
                ..Derives::NONE
            },
            TypeRef::String => Derives {
                copy: false,
                ..Derives::ALL
            },
            TypeRef::Vec(inner) | TypeRef::Option(inner) => Derives {
                copy: false,
                ..self.of_ref(inner)
            },
            TypeRef::Named(name) if name == NATIVE_HANDLE_ID => Derives::ALL,
            TypeRef::Named(name) => self
                .derives
                .get(name.as_str())
                .copied()
                .unwrap_or(Derives::NONE),
            TypeRef::Unit
            | TypeRef::Bool
            | TypeRef::U8
            | TypeRef::I8
            | TypeRef::U16
            | TypeRef::I16
            | TypeRef::U32
            | TypeRef::I32
            | TypeRef::U64
            | TypeRef::I64 => Derives::ALL,
        }
    }
}

fn rust_ty(ty: &TypeRef) -> String {
    match ty {
        TypeRef::Unit => "()".to_owned(),
        TypeRef::Bool => "bool".to_owned(),
        TypeRef::U8 => "u8".to_owned(),
        TypeRef::I8 => "i8".to_owned(),
        TypeRef::U16 => "u16".to_owned(),
        TypeRef::I16 => "i16".to_owned(),
        TypeRef::U32 => "u32".to_owned(),
        TypeRef::I32 => "i32".to_owned(),
        TypeRef::U64 => "u64".to_owned(),
        TypeRef::I64 => "i64".to_owned(),
        TypeRef::F32 => "f32".to_owned(),
        TypeRef::F64 => "f64".to_owned(),
        TypeRef::String => "String".to_owned(),
        TypeRef::Bytes => "Vec<u8>".to_owned(),
        TypeRef::Vec(inner) => format!("Vec<{}>", rust_ty(inner)),
        TypeRef::Option(inner) => format!("Option<{}>", rust_ty(inner)),
        TypeRef::Named(name) => name.clone(),
    }
}

fn to_rust_field(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{EnumVariant, Field};

    fn contract(types: Vec<TypeDef>) -> Contract {
        Contract {
            plugin_id: "x".to_owned(),
            type_name: "X".to_owned(),
            methods: vec![],
            init: None,
            types,
        }
    }

    #[test]
    fn recursive_types_terminate() {
        let node = TypeDef::Struct(StructDef {
            name: "Node".to_owned(),
            fields: vec![
                Field {
                    name: "id".to_owned(),
                    ty: TypeRef::U32,
                },
                Field {
                    name: "children".to_owned(),
                    ty: TypeRef::Vec(Box::new(TypeRef::Named("Node".to_owned()))),
                },
            ],
        });
        let out = generate_rust_types(&contract(vec![node]));
        assert!(out.contains("#[derive(Debug, Clone, PartialEq, Eq, Hash)]\npub struct Node"));
    }

    #[test]
    fn multi_payload_variants_keep_every_field() {
        let e = TypeDef::Enum(EnumDef {
            name: "E".to_owned(),
            variants: vec![EnumVariant {
                name: "Pair".to_owned(),
                payload: vec![TypeRef::U8, TypeRef::String],
            }],
        });
        let out = generate_rust_types(&contract(vec![e]));
        assert!(out.contains("    Pair(u8, String),"));
    }
}
