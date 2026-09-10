//! Build-time extraction of a [`Contract`] from a plugin crate's Rust source.
//!
//! The plugin author writes the trait once under `#[istmo::plugin]` and its
//! wire types under `#[istmo::message]`; the `build.rs` script calls
//! [`extract_contract`] which parses the same source file with `syn`,
//! walks the trait declaration and any sibling `#[istmo::message]` items,
//! and returns a fully-formed [`Contract`] suitable for `emit_contract` and
//! for the Kotlin / Swift generators.
//!
//! This removes the "two sources of truth" problem: the trait signature is
//! now the only place the wire shape is described.

use std::fs;
use std::io;
use std::path::Path;

use syn::punctuated::Punctuated;
use syn::{
    AngleBracketedGenericArguments, Attribute, Expr, ExprLit, ExprPath, File, FnArg,
    GenericArgument, Item, ItemEnum, ItemStruct, ItemTrait, Lit, MetaNameValue, PathArguments,
    ReturnType, Token, TraitItem, TraitItemFn, Type, TypePath,
};

use crate::contract::{
    Arg, Contract, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef, TypeDef, TypeRef,
};

/// Failure returned by [`extract_contract`].
#[derive(Debug)]
pub enum ExtractError {
    /// Could not read the source file.
    Io(io::Error),
    /// The source file did not parse as valid Rust.
    Parse(syn::Error),
    /// No `#[istmo::plugin]` trait with the requested name was found.
    TraitNotFound(String),
    /// Trait shape could not be lowered to the contract data model.
    Unsupported(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading plugin source: {e}"),
            Self::Parse(e) => write!(f, "parsing plugin source: {e}"),
            Self::TraitNotFound(name) => {
                write!(f, "no `#[istmo::plugin]` trait named `{name}` in source")
            }
            Self::Unsupported(msg) => write!(f, "unsupported trait shape: {msg}"),
        }
    }
}

impl std::error::Error for ExtractError {}

impl From<io::Error> for ExtractError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<syn::Error> for ExtractError {
    fn from(e: syn::Error) -> Self {
        Self::Parse(e)
    }
}

/// Parse `source_path` and lower the `#[istmo::plugin] trait <trait_name>`
/// declaration (plus every `#[istmo::message]` item in the same file) into
/// a [`Contract`].
///
/// # Errors
/// See [`ExtractError`] variants.
pub fn extract_contract(
    source_path: impl AsRef<Path>,
    trait_name: &str,
) -> Result<Contract, ExtractError> {
    let source = fs::read_to_string(source_path.as_ref())?;
    let file: File = syn::parse_file(&source)?;
    lower_contract(&file, trait_name)
}

fn lower_contract(file: &File, trait_name: &str) -> Result<Contract, ExtractError> {
    let (trait_def, attr_args) = find_plugin_trait(file, trait_name)?;
    let plugin_id = attr_args
        .plugin_id
        .ok_or_else(|| ExtractError::Unsupported("plugin attribute missing `name = \"…\"`".into()))?;
    let type_name = trait_def.ident.to_string();

    let mut methods = Vec::new();
    for item in &trait_def.items {
        let TraitItem::Fn(func) = item else { continue };
        methods.push(lower_method(func)?);
    }

    let types = collect_message_types(file)?;

    Ok(Contract {
        plugin_id,
        type_name,
        methods,
        init: attr_args.init,
        types,
    })
}

struct PluginAttrArgs {
    plugin_id: Option<String>,
    init: Option<TypeRef>,
}

fn find_plugin_trait<'f>(
    file: &'f File,
    trait_name: &str,
) -> Result<(&'f ItemTrait, PluginAttrArgs), ExtractError> {
    for item in &file.items {
        let Item::Trait(t) = item else { continue };
        if t.ident != trait_name {
            continue;
        }
        for attr in &t.attrs {
            if !is_istmo_attr(attr, "plugin") {
                continue;
            }
            let args = parse_plugin_attr(attr)?;
            return Ok((t, args));
        }
    }
    Err(ExtractError::TraitNotFound(trait_name.to_owned()))
}

fn parse_plugin_attr(attr: &Attribute) -> Result<PluginAttrArgs, ExtractError> {
    let pairs = attr
        .parse_args_with(Punctuated::<MetaNameValue, Token![,]>::parse_terminated)
        .map_err(ExtractError::Parse)?;
    let mut plugin_id = None;
    let mut init = None;
    for pair in pairs {
        let key = pair
            .path
            .get_ident()
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        match key.as_str() {
            "name" => {
                plugin_id = Some(expect_str(&pair.value)?);
            }
            "init" => {
                let ty = expect_type(&pair.value)?;
                init = Some(type_to_ref(&ty)?);
            }
            "crate" | "bincode" => {} // irrelevant to contract shape
            _ => {} // forward-compat: ignore unknown keys
        }
    }
    Ok(PluginAttrArgs { plugin_id, init })
}

fn expect_str(expr: &Expr) -> Result<String, ExtractError> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.value()),
        _ => Err(ExtractError::Unsupported("expected string literal".into())),
    }
}

fn expect_type(expr: &Expr) -> Result<Type, ExtractError> {
    match expr {
        Expr::Path(ExprPath { path, .. }) => Ok(Type::Path(TypePath {
            qself: None,
            path: path.clone(),
        })),
        _ => Err(ExtractError::Unsupported(
            "expected a type path (e.g. `init = SignInConfig`)".into(),
        )),
    }
}

fn lower_method(func: &TraitItemFn) -> Result<Method, ExtractError> {
    let name = func.sig.ident.to_string();
    let kind = if func.attrs.iter().any(|a| is_istmo_attr(a, "stream")) {
        MethodKind::Stream
    } else {
        MethodKind::Unary
    };

    let mut args = Vec::new();
    for input in &func.sig.inputs {
        let FnArg::Typed(pat) = input else { continue };
        let ty = &*pat.ty;
        // Convention (per CLAUDE.md): args whose leaf type is `CancelToken`
        // are cooperative-cancellation opt-ins — the macro fills them from
        // the runtime and strips them from the wire. Do the same here.
        if is_cancel_token(ty) {
            continue;
        }
        let name = pat_name(&pat.pat).unwrap_or_else(|| format!("arg{}", args.len()));
        args.push(Arg {
            name,
            ty: type_to_ref(ty)?,
        });
    }

    let (returns, error) = match &func.sig.output {
        ReturnType::Default => (TypeRef::Unit, None),
        ReturnType::Type(_, ty) => split_result(ty)?,
    };

    Ok(Method {
        name,
        kind,
        args,
        returns,
        error,
    })
}

fn split_result(ty: &Type) -> Result<(TypeRef, Option<TypeRef>), ExtractError> {
    if let Type::Path(TypePath { path, .. }) = ty
        && let Some(seg) = path.segments.last()
        && seg.ident == "Result"
        && let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) =
            &seg.arguments
    {
        let mut it = args.iter();
        let ok = it.next().and_then(generic_type);
        let err = it.next().and_then(generic_type);
        if let (Some(ok), Some(err)) = (ok, err) {
            return Ok((type_to_ref(ok)?, Some(type_to_ref(err)?)));
        }
    }
    Ok((type_to_ref(ty)?, None))
}

fn generic_type(arg: &GenericArgument) -> Option<&Type> {
    match arg {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

fn type_to_ref(ty: &Type) -> Result<TypeRef, ExtractError> {
    match ty {
        Type::Tuple(t) if t.elems.is_empty() => Ok(TypeRef::Unit),
        Type::Path(TypePath { path, .. }) => {
            let Some(seg) = path.segments.last() else {
                return Err(ExtractError::Unsupported(format!(
                    "empty type path in `{}`",
                    tokens(ty)
                )));
            };
            let name = seg.ident.to_string();
            match name.as_str() {
                "bool" => Ok(TypeRef::Bool),
                "u8" => Ok(TypeRef::U8),
                "i8" => Ok(TypeRef::I8),
                "u16" => Ok(TypeRef::U16),
                "i16" => Ok(TypeRef::I16),
                "u32" => Ok(TypeRef::U32),
                "i32" => Ok(TypeRef::I32),
                "u64" => Ok(TypeRef::U64),
                "i64" => Ok(TypeRef::I64),
                "f32" => Ok(TypeRef::F32),
                "f64" => Ok(TypeRef::F64),
                "String" => Ok(TypeRef::String),
                "Vec" => {
                    let inner = generic_arg_ty(&seg.arguments, "Vec")?;
                    // `Vec<u8>` collapses to `Bytes` so Kotlin surfaces
                    // `ByteArray` and Swift `Data`.
                    if is_u8(inner) {
                        Ok(TypeRef::Bytes)
                    } else {
                        Ok(TypeRef::Vec(Box::new(type_to_ref(inner)?)))
                    }
                }
                "Option" => {
                    let inner = generic_arg_ty(&seg.arguments, "Option")?;
                    Ok(TypeRef::Option(Box::new(type_to_ref(inner)?)))
                }
                _ => Ok(TypeRef::Named(name)),
            }
        }
        _ => Err(ExtractError::Unsupported(format!(
            "type not supported: `{}`",
            tokens(ty)
        ))),
    }
}

fn generic_arg_ty<'a>(args: &'a PathArguments, wrapper: &str) -> Result<&'a Type, ExtractError> {
    let PathArguments::AngleBracketed(AngleBracketedGenericArguments { args, .. }) = args else {
        return Err(ExtractError::Unsupported(format!(
            "`{wrapper}` used without a type parameter"
        )));
    };
    args.iter()
        .find_map(generic_type)
        .ok_or_else(|| ExtractError::Unsupported(format!("`{wrapper}<…>` missing type argument")))
}

fn is_u8(ty: &Type) -> bool {
    matches!(ty, Type::Path(TypePath { path, .. })
        if path.segments.last().is_some_and(|s| s.ident == "u8"))
}

fn is_cancel_token(ty: &Type) -> bool {
    matches!(ty, Type::Path(TypePath { path, .. })
        if path.segments.last().is_some_and(|s| s.ident == "CancelToken"))
}

fn pat_name(pat: &syn::Pat) -> Option<String> {
    match pat {
        syn::Pat::Ident(id) => Some(id.ident.to_string()),
        _ => None,
    }
}

fn collect_message_types(file: &File) -> Result<Vec<TypeDef>, ExtractError> {
    let mut out = Vec::new();
    for item in &file.items {
        match item {
            Item::Struct(s) if has_message_attr(&s.attrs) => {
                out.push(TypeDef::Struct(lower_struct(s)?));
            }
            Item::Enum(e) if has_message_attr(&e.attrs) => {
                out.push(TypeDef::Enum(lower_enum(e)?));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn has_message_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| is_istmo_attr(a, "message"))
}

fn lower_struct(s: &ItemStruct) -> Result<StructDef, ExtractError> {
    let syn::Fields::Named(named) = &s.fields else {
        return Err(ExtractError::Unsupported(format!(
            "`#[istmo::message]` on `{}`: only named-field structs are supported",
            s.ident
        )));
    };
    let mut fields = Vec::with_capacity(named.named.len());
    for f in &named.named {
        let name = f
            .ident
            .as_ref()
            .ok_or_else(|| {
                ExtractError::Unsupported(format!(
                    "unnamed field in `#[istmo::message]` struct `{}`",
                    s.ident
                ))
            })?
            .to_string();
        // Contract field names travel to Kotlin / Swift, both of which
        // idiomatically use `camelCase`. Rust source is `snake_case`, so
        // fold that convention here — the wire is field-order-based, not
        // name-based, so this rename is cosmetic only. Fields already
        // written in `camelCase` in the Rust source are left as-is.
        let wire_name = snake_to_camel(&name);
        fields.push(Field {
            name: wire_name,
            ty: type_to_ref(&f.ty)?,
        });
    }
    Ok(StructDef {
        name: s.ident.to_string(),
        fields,
    })
}

fn lower_enum(e: &ItemEnum) -> Result<EnumDef, ExtractError> {
    let mut variants = Vec::with_capacity(e.variants.len());
    for v in &e.variants {
        let payload = match &v.fields {
            syn::Fields::Unit => Vec::new(),
            syn::Fields::Unnamed(u) => u
                .unnamed
                .iter()
                .map(|f| type_to_ref(&f.ty))
                .collect::<Result<Vec<_>, _>>()?,
            syn::Fields::Named(_) => {
                return Err(ExtractError::Unsupported(format!(
                    "`#[istmo::message]` enum `{}::{}`: named-field variants unsupported",
                    e.ident, v.ident
                )));
            }
        };
        variants.push(EnumVariant {
            name: v.ident.to_string(),
            payload,
        });
    }
    Ok(EnumDef {
        name: e.ident.to_string(),
        variants,
    })
}

/// Recognises `#[foo]`, `#[istmo::foo]`, `#[::istmo::foo]`.
fn is_istmo_attr(attr: &Attribute, tail: &str) -> bool {
    let segs: Vec<String> = attr
        .path()
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    match segs.as_slice() {
        [only] => only == tail,
        [ns, name] => (ns == "istmo" || ns == "istmo_macros") && name == tail,
        _ => false,
    }
}

/// `foo_bar_baz` → `fooBarBaz`. Names already in `camelCase` (no
/// underscore) pass through unchanged. Leading underscore preserved.
fn snake_to_camel(name: &str) -> String {
    if !name.contains('_') {
        return name.to_owned();
    }
    let mut out = String::with_capacity(name.len());
    let mut upper_next = false;
    for (i, ch) in name.chars().enumerate() {
        if ch == '_' {
            if i == 0 {
                out.push('_');
            } else {
                upper_next = true;
            }
        } else if upper_next {
            out.extend(ch.to_uppercase());
            upper_next = false;
        } else {
            out.push(ch);
        }
    }
    out
}

fn tokens(ty: &Type) -> String {
    use quote::ToTokens;
    let mut s = proc_macro2::TokenStream::new();
    ty.to_tokens(&mut s);
    s.to_string()
}
