//! `#[istmo::message]` attribute macro.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, Item, ItemStruct, Lit, LitStr, MetaNameValue, Path, Token,
    parse_quote, parse_str, parse2,
};

#[allow(unreachable_pub)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: MessageArgs = parse2(attr)?;
    let item: Item = parse2(item)?;
    match item {
        Item::Struct(s) => expand_struct(&args, s),
        Item::Enum(e) => Ok(expand_enum(&args, &e)),
        other => Err(syn::Error::new_spanned(
            other,
            "#[istmo::message] can only be applied to struct or enum declarations",
        )),
    }
}

fn expand_enum(args: &MessageArgs, item: &syn::ItemEnum) -> TokenStream {
    let bincode = &args.bincode_path;
    quote! {
        #[derive(#bincode::Encode, #bincode::Decode)]
        #item
    }
}

/// Struct expansion. If any field carries `#[handle(Marker)]`, emits a
/// sibling `Owned<Name>` with the handles adopted into `NativeHandle<M>`
/// plus an `into_owned(rt)` adapter. Otherwise behaves like the enum path.
fn expand_struct(args: &MessageArgs, mut wire: ItemStruct) -> syn::Result<TokenStream> {
    let handles = collect_handle_fields(&wire)?;
    strip_handle_attrs(&mut wire);

    let bincode = &args.bincode_path;
    let wire_tokens = quote! {
        #[derive(#bincode::Encode, #bincode::Decode)]
        #wire
    };

    if handles.is_empty() {
        return Ok(wire_tokens);
    }

    let root = &args.crate_path;
    let owned = generate_owned_struct(&wire, &handles, root);
    let adapter = generate_into_owned(&wire, &handles, root);

    Ok(quote! {
        #wire_tokens
        #owned
        #adapter
    })
}

struct HandleField {
    /// Position of the field in the struct's field list.
    index: usize,
    /// Marker type from `#[handle(Marker)]`.
    marker: syn::Type,
}

/// Walk struct fields, extract `#[handle(Marker)]` entries. Rejects tuple
/// / unit structs — v1 only supports named fields.
fn collect_handle_fields(s: &ItemStruct) -> syn::Result<Vec<HandleField>> {
    let syn::Fields::Named(named) = &s.fields else {
        // No named fields → cannot carry `#[handle]`. Silently ignore
        // (empty result); the caller falls back to the pure-wire path.
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, f) in named.named.iter().enumerate() {
        let Some(marker) = handle_marker(&f.attrs)? else {
            continue;
        };
        if f.ident.is_none() {
            return Err(syn::Error::new_spanned(f, "`#[handle]` requires a named field"));
        }
        out.push(HandleField { index, marker });
    }
    Ok(out)
}

/// Parse `#[handle(Marker)]` on a field's attribute list. Returns `Some(Marker)`
/// once found; `None` when absent. Rejects `#[handle]` (no arg) explicitly.
fn handle_marker(attrs: &[Attribute]) -> syn::Result<Option<syn::Type>> {
    for attr in attrs {
        if !is_handle_attr(attr) {
            continue;
        }
        let marker: syn::Type = attr
            .parse_args()
            .map_err(|e| syn::Error::new_spanned(attr, format!("`#[handle(...)]`: {e}")))?;
        return Ok(Some(marker));
    }
    Ok(None)
}

fn is_handle_attr(attr: &Attribute) -> bool {
    let segs: Vec<String> = attr
        .path()
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    match segs.as_slice() {
        [only] => only == "handle",
        [ns, name] => (ns == "istmo" || ns == "istmo_macros") && name == "handle",
        _ => false,
    }
}

fn strip_handle_attrs(s: &mut ItemStruct) {
    if let syn::Fields::Named(named) = &mut s.fields {
        for f in &mut named.named {
            f.attrs.retain(|a| !is_handle_attr(a));
        }
    }
}

/// Build the `Owned<Name>` sibling struct — same fields as `wire`, but
/// every handle field has its type swapped to `NativeHandle<Marker>`.
///
/// Derives are intentionally minimal: `NativeHandle` is not `Clone` /
/// `Copy` / `PartialEq` (see `CLAUDE.md` M6 notes), so only `Debug` is
/// safe here. Users that need more implement them manually on the
/// generated `Owned` type.
fn generate_owned_struct(wire: &ItemStruct, handles: &[HandleField], root: &Path) -> TokenStream {
    let vis = &wire.vis;
    let owned_ident = format_ident!("Owned{}", wire.ident);
    let syn::Fields::Named(named) = &wire.fields else {
        return quote! {}; // unreachable for handles.len() > 0
    };
    let fields = named.named.iter().enumerate().map(|(idx, f)| {
        let field_vis = &f.vis;
        let name = f.ident.as_ref().expect("named field");
        if let Some(h) = handles.iter().find(|h| h.index == idx) {
            let marker = &h.marker;
            quote! {
                #field_vis #name: #root::NativeHandle<#marker>
            }
        } else {
            let ty = &f.ty;
            quote! {
                #field_vis #name: #ty
            }
        }
    });
    quote! {
        #[derive(::core::fmt::Debug)]
        #vis struct #owned_ident {
            #(#fields,)*
        }
    }
}

/// Emit `impl <Wire> { pub fn into_owned(self, rt: &Arc<Runtime>) -> Owned<Wire> { ... } }`.
fn generate_into_owned(wire: &ItemStruct, handles: &[HandleField], root: &Path) -> TokenStream {
    let wire_ident = &wire.ident;
    let owned_ident = format_ident!("Owned{}", wire_ident);
    let syn::Fields::Named(named) = &wire.fields else {
        return quote! {};
    };
    let field_assigns = named.named.iter().enumerate().map(|(idx, f)| {
        let name = f.ident.as_ref().expect("named field");
        if let Some(h) = handles.iter().find(|h| h.index == idx) {
            let marker = &h.marker;
            quote! {
                #name: #root::NativeHandle::<#marker>::adopt(rt, self.#name)
            }
        } else {
            quote! {
                #name: self.#name
            }
        }
    });
    quote! {
        impl #wire_ident {
            /// Adopt every `#[handle]` field into a typed
            /// [`NativeHandle`](#root::NativeHandle) tied to `rt`.
            /// Dropping the returned owned wrapper fires
            /// `Frame::ReleaseNativeHandle` on the native side.
            #[must_use]
            pub fn into_owned(
                self,
                rt: &::std::sync::Arc<#root::Runtime>,
            ) -> #owned_ident {
                #owned_ident {
                    #(#field_assigns,)*
                }
            }
        }
    }
}

struct MessageArgs {
    bincode_path: Path,
    crate_path: Path,
}

impl syn::parse::Parse for MessageArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut bincode_path: Option<Path> = None;
        let mut crate_path: Option<Path> = None;
        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier"))?
                .to_string();
            match key.as_str() {
                "bincode" => {
                    let raw = expect_lit_str(&pair.value)?;
                    bincode_path = Some(parse_str::<Path>(&raw.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `bincode` path: {e}"))
                    })?);
                }
                "crate" => {
                    let raw = expect_lit_str(&pair.value)?;
                    crate_path = Some(parse_str::<Path>(&raw.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `crate` path: {e}"))
                    })?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!(
                            "unknown argument `{other}`; expected `bincode` or `crate`"
                        ),
                    ));
                }
            }
        }
        Ok(Self {
            bincode_path: bincode_path.unwrap_or_else(|| parse_quote!(::istmo::bincode)),
            crate_path: crate_path.unwrap_or_else(|| parse_quote!(::istmo)),
        })
    }
}

fn expect_lit_str(expr: &Expr) -> syn::Result<LitStr> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) => Ok(s.clone()),
        _ => Err(syn::Error::new_spanned(expr, "expected string literal")),
    }
}
