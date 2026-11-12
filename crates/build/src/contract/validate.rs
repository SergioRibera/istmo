//! Shape checks a [`Contract`] must pass before the generators see it.
//!
//! The generators assume every named type resolves and that nesting
//! stays within what the Kotlin / Swift codecs can express. Checking
//! once here turns what would be a confusing compile error in generated
//! native code into a build-script error naming the Rust item at fault.

use std::collections::BTreeSet;
use std::fmt;

use super::{Contract, TypeDef, TypeRef};

/// Named type the runtime provides on every platform.
const BUILTIN_NAMED: &[&str] = &["NativeHandleId"];

/// Why a [`Contract`] cannot be generated. `site` names the offending
/// item, e.g. ``method `m` argument `x` ``.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ContractError {
    /// A named type is neither a declared `#[istmo::message]` type nor
    /// runtime-provided.
    UnknownType { site: String, name: String },
    /// `Option<Option<T>>`: Kotlin and Swift collapse the two `None`s.
    NestedOption { site: String },
    /// `()` inside a `Vec` / `Option` carries no data and has no codec.
    UnitInContainer { site: String },
    /// A domain error that is not a declared message type.
    ErrorNotNamed { method: String },
    /// Two declared types share a name.
    DuplicateType { name: String },
    /// Two fields of one struct share a (camelCase) wire name.
    DuplicateField { ty: String, field: String },
}

impl fmt::Display for ContractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownType { site, name } => write!(
                f,
                "{site} uses `{name}`, which is neither a supported primitive nor an \
                 `#[istmo::message]` type of this crate"
            ),
            Self::NestedOption { site } => write!(
                f,
                "{site} nests `Option<Option<_>>`, which Kotlin and Swift cannot tell apart \
                 from a single `Option`; use an enum"
            ),
            Self::UnitInContainer { site } => {
                write!(f, "{site} puts `()` inside a `Vec` or `Option`")
            }
            Self::ErrorNotNamed { method } => write!(
                f,
                "method `{method}` returns `Result<_, E>` where `E` is not an \
                 `#[istmo::message]` type; domain errors must be message types"
            ),
            Self::DuplicateType { name } => {
                write!(f, "two `#[istmo::message]` types are named `{name}`")
            }
            Self::DuplicateField { ty, field } => write!(
                f,
                "struct `{ty}` has two fields named `{field}` once converted to camelCase"
            ),
        }
    }
}

impl std::error::Error for ContractError {}

impl Contract {
    /// Checks the contract only uses shapes the generators support.
    ///
    /// # Errors
    ///
    /// The first [`ContractError`] found, in declaration order.
    pub fn validate(&self) -> Result<(), ContractError> {
        let mut declared = BTreeSet::new();
        for def in &self.types {
            if !declared.insert(def.name()) {
                return Err(ContractError::DuplicateType {
                    name: def.name().to_owned(),
                });
            }
        }
        let check = |site: String, ty: &TypeRef| check_type(&declared, &site, ty, false);

        if let Some(init) = &self.init {
            check("plugin `init`".to_owned(), init)?;
        }
        for m in &self.methods {
            for arg in &m.args {
                check(
                    format!("method `{}` argument `{}`", m.name, arg.name),
                    &arg.ty,
                )?;
            }
            check(format!("method `{}` return type", m.name), &m.returns)?;
            if let Some(err) = &m.error {
                if !matches!(err, TypeRef::Named(_)) {
                    return Err(ContractError::ErrorNotNamed {
                        method: m.name.clone(),
                    });
                }
                check(format!("method `{}` error type", m.name), err)?;
            }
        }
        for def in &self.types {
            match def {
                TypeDef::Struct(s) => {
                    let mut seen = BTreeSet::new();
                    for field in &s.fields {
                        if !seen.insert(field.name.as_str()) {
                            return Err(ContractError::DuplicateField {
                                ty: s.name.clone(),
                                field: field.name.clone(),
                            });
                        }
                        check(
                            format!("type `{}` field `{}`", s.name, field.name),
                            &field.ty,
                        )?;
                    }
                }
                TypeDef::Enum(e) => {
                    for v in &e.variants {
                        for ty in &v.payload {
                            check(format!("type `{}` variant `{}`", e.name, v.name), ty)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }
}

fn check_type(
    declared: &BTreeSet<&str>,
    site: &str,
    ty: &TypeRef,
    in_container: bool,
) -> Result<(), ContractError> {
    match ty {
        TypeRef::Named(name) => {
            if declared.contains(name.as_str()) || BUILTIN_NAMED.contains(&name.as_str()) {
                Ok(())
            } else {
                Err(ContractError::UnknownType {
                    site: site.to_owned(),
                    name: name.clone(),
                })
            }
        }
        TypeRef::Option(inner) if matches!(**inner, TypeRef::Option(_)) => {
            Err(ContractError::NestedOption {
                site: site.to_owned(),
            })
        }
        TypeRef::Vec(inner) | TypeRef::Option(inner) => check_type(declared, site, inner, true),
        TypeRef::Unit if in_container => Err(ContractError::UnitInContainer {
            site: site.to_owned(),
        }),
        TypeRef::Unit
        | TypeRef::Bool
        | TypeRef::U8
        | TypeRef::I8
        | TypeRef::U16
        | TypeRef::I16
        | TypeRef::U32
        | TypeRef::I32
        | TypeRef::U64
        | TypeRef::I64
        | TypeRef::F32
        | TypeRef::F64
        | TypeRef::String
        | TypeRef::Bytes => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contract::{Arg, Field, Method, MethodKind, StructDef};

    fn contract(args: Vec<Arg>, types: Vec<TypeDef>) -> Contract {
        Contract {
            plugin_id: "x".to_owned(),
            type_name: "X".to_owned(),
            methods: vec![Method {
                name: "go".to_owned(),
                kind: MethodKind::Unary,
                args,
                returns: TypeRef::Unit,
                error: None,
            }],
            init: None,
            types,
        }
    }

    fn arg(ty: TypeRef) -> Vec<Arg> {
        vec![Arg {
            name: "a".to_owned(),
            ty,
        }]
    }

    #[test]
    fn unknown_named_type_names_the_site() {
        let err = contract(arg(TypeRef::Named("usize".to_owned())), vec![])
            .validate()
            .unwrap_err();
        assert_eq!(
            err,
            ContractError::UnknownType {
                site: "method `go` argument `a`".to_owned(),
                name: "usize".to_owned(),
            }
        );
        assert!(err.to_string().contains("`usize`"));
    }

    #[test]
    fn builtin_and_declared_names_resolve() {
        let types = vec![TypeDef::Struct(StructDef {
            name: "P".to_owned(),
            fields: vec![Field {
                name: "h".to_owned(),
                ty: TypeRef::Named("NativeHandleId".to_owned()),
            }],
        })];
        assert_eq!(
            contract(arg(TypeRef::Named("P".to_owned())), types).validate(),
            Ok(())
        );
    }

    #[test]
    fn nested_option_and_unit_in_container_are_rejected() {
        let nested = TypeRef::Vec(Box::new(TypeRef::Option(Box::new(TypeRef::Option(
            Box::new(TypeRef::U8),
        )))));
        assert!(matches!(
            contract(arg(nested), vec![]).validate(),
            Err(ContractError::NestedOption { .. })
        ));
        let unit = TypeRef::Vec(Box::new(TypeRef::Unit));
        assert!(matches!(
            contract(arg(unit), vec![]).validate(),
            Err(ContractError::UnitInContainer { .. })
        ));
    }

    #[test]
    fn duplicate_fields_are_rejected() {
        let field = || Field {
            name: "fooBar".to_owned(),
            ty: TypeRef::U8,
        };
        let types = vec![TypeDef::Struct(StructDef {
            name: "S".to_owned(),
            fields: vec![field(), field()],
        })];
        assert!(matches!(
            contract(vec![], types).validate(),
            Err(ContractError::DuplicateField { .. })
        ));
    }
}
