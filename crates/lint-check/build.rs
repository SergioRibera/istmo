//! Generates wire types the way a plugin crate's build script does.

#![deny(warnings)]
#![expect(
    clippy::expect_used,
    reason = "a build script reports failure by panicking"
)]

use std::path::PathBuf;

use istmo_build::{Contract, EnumDef, EnumVariant, Field, StructDef, TypeDef, TypeRef};

fn main() {
    let out = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set for build scripts"));
    let contract = Contract {
        plugin_id: "lint.generated".to_owned(),
        type_name: "Generated".to_owned(),
        methods: Vec::new(),
        init: None,
        types: vec![
            TypeDef::Struct(StructDef {
                name: "Sample".to_owned(),
                fields: vec![
                    Field {
                        name: "count".to_owned(),
                        ty: TypeRef::U32,
                    },
                    Field {
                        name: "labelText".to_owned(),
                        ty: TypeRef::Option(Box::new(TypeRef::String)),
                    },
                    Field {
                        name: "kind".to_owned(),
                        ty: TypeRef::Named("Kind".to_owned()),
                    },
                ],
            }),
            TypeDef::Enum(EnumDef {
                name: "Kind".to_owned(),
                variants: vec![
                    EnumVariant {
                        name: "Plain".to_owned(),
                        payload: Vec::new(),
                    },
                    EnumVariant {
                        name: "Scaled".to_owned(),
                        payload: vec![TypeRef::F64],
                    },
                ],
            }),
        ],
    };
    let source = istmo_build::generate_rust_types(&contract);
    std::fs::write(out.join("generated_types.rs"), source).expect("OUT_DIR is writable");
    println!("cargo::rerun-if-changed=build.rs");
}
