use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use syn::ext::IdentExt as _;
use syn::punctuated::Punctuated;
use syn::{
    AngleBracketedGenericArguments, Attribute, Expr, ExprLit, ExprPath, File, FnArg,
    GenericArgument, Ident, Item, ItemEnum, ItemMod, ItemStruct, ItemTrait, Lit, Meta,
    MetaNameValue, PathArguments, ReturnType, Token, TraitItem, TraitItemFn, Type, TypePath,
};

use crate::contract::{
    Arg, Contract, ContractError, EnumDef, EnumVariant, Field, Method, MethodKind, StructDef,
    TypeDef, TypeRef,
};

#[derive(Debug)]
pub enum ExtractError {
    Io(io::Error),

    Parse(syn::Error),

    TraitNotFound(String),

    Unsupported(String),
}

impl std::fmt::Display for ExtractError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(e) => write!(f, "reading plugin source: {e}"),
            Self::Parse(e) => {
                let start = e.span().start();
                write!(
                    f,
                    "parsing plugin source at line {}, column {}: {e}",
                    start.line,
                    start.column + 1
                )
            }
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

impl From<ContractError> for ExtractError {
    fn from(e: ContractError) -> Self {
        Self::Unsupported(e.to_string())
    }
}

/// Derives the [`Contract`] of the `#[istmo::plugin]` trait `trait_name`
/// declared in `source_path`.
///
/// Message types come from that file and from the modules it declares,
/// inline or in their own files. The result is checked with
/// [`Contract::validate`].
pub fn extract_contract(
    source_path: impl AsRef<Path>,
    trait_name: &str,
) -> Result<Contract, ExtractError> {
    let path = source_path.as_ref();
    let source = fs::read_to_string(path)?;
    let file: File = syn::parse_file(&source)?;
    let contract = lower_contract(&file, trait_name, &ModuleDirs::of_file(path, false))?;
    contract.validate()?;
    Ok(contract)
}

/// Where rustc looks for the files of `mod foo;` items at one nesting
/// level of a source file.
#[derive(Debug, Clone)]
struct ModuleDirs {
    /// Directory of `foo.rs` / `foo/mod.rs`.
    modules: PathBuf,
    /// Directory a `#[path = "…"]` is relative to.
    path_attr: PathBuf,
}

impl ModuleDirs {
    /// Top level of `path`. A "mod-rs" file (`lib.rs`, `main.rs`,
    /// `mod.rs`, or any file loaded through `#[path]`) keeps its
    /// submodules beside it; any other `foo.rs` keeps them in `foo/`.
    /// `#[path]` is relative to the file's own directory either way.
    fn of_file(path: &Path, mod_rs: bool) -> Self {
        let parent = path.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
        let stem = path.file_stem().and_then(|s| s.to_str());
        let modules = match stem {
            Some(stem) if !mod_rs && !matches!(stem, "lib" | "main" | "mod") => parent.join(stem),
            _ => parent.clone(),
        };
        Self {
            modules,
            path_attr: parent,
        }
    }

    /// Inside the inline `mod name { … }`: both lookups gain `name` as a
    /// directory component.
    fn inline(&self, name: &str) -> Self {
        let dir = self.modules.join(name);
        Self {
            modules: dir.clone(),
            path_attr: dir,
        }
    }
}

fn lower_contract(
    file: &File,
    trait_name: &str,
    dirs: &ModuleDirs,
) -> Result<Contract, ExtractError> {
    let (trait_def, attr_args) = find_plugin_trait(&file.items, trait_name)?
        .ok_or_else(|| ExtractError::TraitNotFound(trait_name.to_owned()))?;
    let plugin_id = attr_args.plugin_id.ok_or_else(|| {
        ExtractError::Unsupported("plugin attribute missing `name = \"…\"`".into())
    })?;
    let type_name = ident(&trait_def.ident);

    let mut methods = Vec::new();
    for item in &trait_def.items {
        let TraitItem::Fn(func) = item else { continue };
        methods.push(lower_method(func)?);
    }

    let mut types = Vec::new();
    collect_message_types(&file.items, dirs, &mut types)?;

    Ok(Contract {
        plugin_id,
        type_name,
        methods,
        init: attr_args.init,
        types,
    })
}

/// An identifier as the generated code spells it: `r#type` → `type`.
fn ident(id: &Ident) -> String {
    id.unraw().to_string()
}

struct PluginAttrArgs {
    plugin_id: Option<String>,
    init: Option<TypeRef>,
}

/// Searches `items` and the inline modules among them.
fn find_plugin_trait<'f>(
    items: &'f [Item],
    trait_name: &str,
) -> Result<Option<(&'f ItemTrait, PluginAttrArgs)>, ExtractError> {
    for item in items {
        match item {
            Item::Trait(t) if t.ident.unraw() == trait_name => {
                if let Some(attr) = t.attrs.iter().find(|a| is_istmo_attr(a, "plugin")) {
                    return Ok(Some((t, parse_plugin_attr(attr)?)));
                }
            }
            Item::Mod(ItemMod {
                content: Some((_, inner)),
                ..
            }) => {
                if let Some(found) = find_plugin_trait(inner, trait_name)? {
                    return Ok(Some(found));
                }
            }
            _ => {}
        }
    }
    Ok(None)
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
                init = Some(type_to_ref(&ty).map_err(|e| in_site(e, "plugin `init`"))?);
            }
            // Codegen paths only; they do not change the contract.
            "crate" | "wincode" => {}
            other => {
                return Err(ExtractError::Unsupported(format!(
                    "unknown `#[istmo::plugin]` argument `{other}`; expected `name`, `init` or `crate`"
                )));
            }
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

/// Prefixes an [`ExtractError::Unsupported`] message with where it occurred.
fn in_site(err: ExtractError, site: &str) -> ExtractError {
    match err {
        ExtractError::Unsupported(msg) => ExtractError::Unsupported(format!("{site}: {msg}")),
        other => other,
    }
}

fn lower_method(func: &TraitItemFn) -> Result<Method, ExtractError> {
    let name = ident(&func.sig.ident);
    let kind = if func.attrs.iter().any(|a| is_istmo_attr(a, "stream")) {
        MethodKind::Stream
    } else {
        MethodKind::Unary
    };

    let mut args = Vec::new();
    for input in &func.sig.inputs {
        let FnArg::Typed(pat) = input else { continue };
        let ty = &*pat.ty;

        if is_cancel_token(ty) {
            continue;
        }
        let arg_name = pat_name(&pat.pat).unwrap_or_else(|| format!("arg{}", args.len()));
        let ty = type_to_ref(ty)
            .map_err(|e| in_site(e, &format!("method `{name}` argument `{arg_name}`")))?;
        args.push(Arg { name: arg_name, ty });
    }

    let (returns, error) = match &func.sig.output {
        ReturnType::Default => (TypeRef::Unit, None),
        ReturnType::Type(_, ty) => {
            split_result(ty).map_err(|e| in_site(e, &format!("method `{name}` return type")))?
        }
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
        let mut it = args.iter().filter_map(generic_type);
        return match (it.next(), it.next(), it.next()) {
            (Some(ok), Some(err), None) => Ok((type_to_ref(ok)?, Some(type_to_ref(err)?))),
            _ => Err(ExtractError::Unsupported(format!(
                "`{}` is not `Result<T, E>`; spell out both type arguments instead of an \
                 alias such as `io::Result<T>`",
                tokens(ty)
            ))),
        };
    }
    Ok((type_to_ref(ty)?, None))
}

const fn generic_type(arg: &GenericArgument) -> Option<&Type> {
    match arg {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    }
}

/// Why a Rust primitive has no wire mapping, `None` when it has one or is
/// not a primitive.
fn unsupported_primitive(name: &str) -> Option<&'static str> {
    Some(match name {
        "usize" | "isize" => "its width depends on the platform; use a fixed-width integer",
        "u128" | "i128" => "128-bit integers have no Kotlin/Swift codec",
        "char" | "str" => "use `String`",
        _ => return None,
    })
}

fn type_to_ref(ty: &Type) -> Result<TypeRef, ExtractError> {
    match ty {
        Type::Tuple(t) if t.elems.is_empty() => Ok(TypeRef::Unit),
        Type::Paren(inner) => type_to_ref(&inner.elem),
        Type::Path(TypePath { path, qself: None }) => {
            let Some(seg) = path.segments.last() else {
                return Err(ExtractError::Unsupported(format!(
                    "empty type path in `{}`",
                    tokens(ty)
                )));
            };
            let name = ident(&seg.ident);
            if let Some(why) = unsupported_primitive(&name) {
                return Err(ExtractError::Unsupported(format!(
                    "type `{name}` is not supported: {why}"
                )));
            }
            let scalar = match name.as_str() {
                "bool" => Some(TypeRef::Bool),
                "u8" => Some(TypeRef::U8),
                "i8" => Some(TypeRef::I8),
                "u16" => Some(TypeRef::U16),
                "i16" => Some(TypeRef::I16),
                "u32" => Some(TypeRef::U32),
                "i32" => Some(TypeRef::I32),
                "u64" => Some(TypeRef::U64),
                "i64" => Some(TypeRef::I64),
                "f32" => Some(TypeRef::F32),
                "f64" => Some(TypeRef::F64),
                "String" => Some(TypeRef::String),
                _ => None,
            };
            if let Some(scalar) = scalar {
                return Ok(scalar);
            }
            match name.as_str() {
                "Vec" => {
                    let inner = generic_arg_ty(&seg.arguments, "Vec")?;
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
                _ if !seg.arguments.is_none() => Err(ExtractError::Unsupported(format!(
                    "generic type `{}` is not supported; only `Vec<T>` and `Option<T>` \
                     map to Kotlin/Swift",
                    tokens(ty)
                ))),
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
        syn::Pat::Ident(id) => Some(ident(&id.ident)),
        _ => None,
    }
}

/// Appends the message types of `items`, descending into inline modules
/// and into `mod foo;` files resolved the way rustc does (`#[path]`,
/// `foo.rs`, `foo/mod.rs`). A module file that does not exist is
/// skipped; a type it would have declared then surfaces as an unknown
/// type in [`Contract::validate`].
fn collect_message_types(
    items: &[Item],
    dirs: &ModuleDirs,
    out: &mut Vec<TypeDef>,
) -> Result<(), ExtractError> {
    for item in items {
        match item {
            Item::Struct(s) if has_message_attr(&s.attrs) => {
                out.push(TypeDef::Struct(lower_struct(s)?));
            }
            Item::Enum(e) if has_message_attr(&e.attrs) => {
                out.push(TypeDef::Enum(lower_enum(e)?));
            }
            Item::Mod(m) => collect_module(m, dirs, out)?,
            _ => {}
        }
    }
    Ok(())
}

fn collect_module(
    m: &ItemMod,
    dirs: &ModuleDirs,
    out: &mut Vec<TypeDef>,
) -> Result<(), ExtractError> {
    let name = ident(&m.ident);
    if let Some((_, items)) = &m.content {
        return collect_message_types(items, &dirs.inline(&name), out);
    }
    let attr = path_attr(&m.attrs);
    let candidates = attr.as_ref().map_or_else(
        || {
            vec![
                dirs.modules.join(format!("{name}.rs")),
                dirs.modules.join(&name).join("mod.rs"),
            ]
        },
        |p| vec![dirs.path_attr.join(p)],
    );
    let Some(file_path) = candidates.into_iter().find(|p| p.is_file()) else {
        return Ok(());
    };
    let source = fs::read_to_string(&file_path)?;
    let file = syn::parse_file(&source)?;
    // rustc treats a file loaded through `#[path]` as mod-rs.
    let child = ModuleDirs::of_file(&file_path, attr.is_some());
    collect_message_types(&file.items, &child, out)
}

fn path_attr(attrs: &[Attribute]) -> Option<String> {
    attrs.iter().find_map(|a| match &a.meta {
        Meta::NameValue(nv) if nv.path.is_ident("path") => match &nv.value {
            Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) => Some(s.value()),
            _ => None,
        },
        _ => None,
    })
}

fn has_message_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| is_istmo_attr(a, "message"))
}

fn lower_struct(s: &ItemStruct) -> Result<StructDef, ExtractError> {
    let struct_name = ident(&s.ident);
    let syn::Fields::Named(named) = &s.fields else {
        return Err(ExtractError::Unsupported(format!(
            "`#[istmo::message]` on `{struct_name}`: only named-field structs are supported"
        )));
    };
    let mut fields = Vec::with_capacity(named.named.len());
    // camelCase wire name → Rust field it came from.
    let mut wire_names: BTreeMap<String, String> = BTreeMap::new();
    for f in &named.named {
        let name = f.ident.as_ref().map(ident).ok_or_else(|| {
            ExtractError::Unsupported(format!(
                "unnamed field in `#[istmo::message]` struct `{struct_name}`"
            ))
        })?;

        let wire_name = snake_to_camel(&name);
        if let Some(previous) = wire_names.insert(wire_name.clone(), name.clone()) {
            return Err(ExtractError::Unsupported(format!(
                "fields `{previous}` and `{name}` of `{struct_name}` both become \
                 `{wire_name}` in Kotlin/Swift"
            )));
        }
        let ty = type_to_ref(&f.ty)
            .map_err(|e| in_site(e, &format!("type `{struct_name}` field `{name}`")))?;
        fields.push(Field {
            name: wire_name,
            ty,
        });
    }
    Ok(StructDef {
        name: struct_name,
        fields,
    })
}

fn lower_enum(e: &ItemEnum) -> Result<EnumDef, ExtractError> {
    let enum_name = ident(&e.ident);
    let mut variants = Vec::with_capacity(e.variants.len());
    for v in &e.variants {
        let variant = ident(&v.ident);
        let payload = match &v.fields {
            syn::Fields::Unit => Vec::new(),
            syn::Fields::Unnamed(u) => u
                .unnamed
                .iter()
                .map(|f| type_to_ref(&f.ty))
                .collect::<Result<Vec<_>, _>>()
                .map_err(|err| in_site(err, &format!("type `{enum_name}` variant `{variant}`")))?,
            syn::Fields::Named(_) => {
                return Err(ExtractError::Unsupported(format!(
                    "`#[istmo::message]` enum `{enum_name}::{variant}`: named-field variants unsupported"
                )));
            }
        };
        variants.push(EnumVariant {
            name: variant,
            payload,
        });
    }
    Ok(EnumDef {
        name: enum_name,
        variants,
    })
}

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
