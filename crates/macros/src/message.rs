use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Fields, Item, ItemStruct, LitStr, MetaNameValue, Path, Token, parse_quote, parse2,
};

use crate::common::expect_path_lit;

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: MessageArgs = parse2(attr)?;
    let item: Item = parse2(item)?;
    match item {
        Item::Struct(s) => expand_struct(&args, s),
        Item::Enum(e) => expand_enum(&args, &e),
        other => Err(syn::Error::new_spanned(
            other,
            "#[istmo::message] can only be applied to struct or enum declarations",
        )),
    }
}

fn expand_enum(args: &MessageArgs, item: &syn::ItemEnum) -> syn::Result<TokenStream> {
    for variant in &item.variants {
        reject_handle_attrs(
            &variant.fields,
            "`#[handle]` is not supported inside enums; carry the handle in a `#[message]` struct field",
        )?;
    }
    let derive = args.derive();
    Ok(quote! {
        #derive
        #item
    })
}

fn expand_struct(args: &MessageArgs, mut wire: ItemStruct) -> syn::Result<TokenStream> {
    if !matches!(wire.fields, Fields::Named(_)) {
        reject_handle_attrs(
            &wire.fields,
            "`#[handle]` requires a struct with named fields",
        )?;
    }
    let handles = collect_handle_fields(&wire)?;
    strip_handle_attrs(&mut wire);

    let derive = args.derive();
    let wire_tokens = quote! {
        #derive
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
    index: usize,

    marker: syn::Type,
}

/// `#[handle]` is an inert marker that only a named-field struct
/// consumes; anywhere else it would reach rustc as an unknown attribute.
fn reject_handle_attrs(fields: &Fields, message: &str) -> syn::Result<()> {
    fields
        .iter()
        .flat_map(|f| &f.attrs)
        .find(|a| is_handle_attr(a))
        .map_or(Ok(()), |attr| Err(syn::Error::new_spanned(attr, message)))
}

fn collect_handle_fields(s: &ItemStruct) -> syn::Result<Vec<HandleField>> {
    let Fields::Named(named) = &s.fields else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, f) in named.named.iter().enumerate() {
        let Some(marker) = handle_marker(&f.attrs)? else {
            continue;
        };
        out.push(HandleField { index, marker });
    }
    Ok(out)
}

fn handle_marker(attrs: &[Attribute]) -> syn::Result<Option<syn::Type>> {
    let mut found = attrs.iter().filter(|a| is_handle_attr(a));
    let Some(attr) = found.next() else {
        return Ok(None);
    };
    if let Some(extra) = found.next() {
        return Err(syn::Error::new_spanned(
            extra,
            "a field takes a single `#[handle(...)]`",
        ));
    }
    let marker: syn::Type = attr
        .parse_args()
        .map_err(|e| syn::Error::new_spanned(attr, format!("`#[handle(...)]`: {e}")))?;
    Ok(Some(marker))
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
    if let Fields::Named(named) = &mut s.fields {
        for f in &mut named.named {
            f.attrs.retain(|a| !is_handle_attr(a));
        }
    }
}

fn generate_owned_struct(wire: &ItemStruct, handles: &[HandleField], root: &Path) -> TokenStream {
    let vis = &wire.vis;
    let owned_ident = format_ident!("Owned{}", wire.ident);
    let generics = &wire.generics;
    let where_clause = &generics.where_clause;
    let Fields::Named(named) = &wire.fields else {
        return quote! {};
    };
    let fields = named.named.iter().enumerate().map(|(idx, f)| {
        let field_vis = &f.vis;
        let name = &f.ident;
        handles.iter().find(|h| h.index == idx).map_or_else(
            || {
                let ty = &f.ty;
                quote! { #field_vis #name: #ty }
            },
            |h| {
                let marker = &h.marker;
                quote! { #field_vis #name: #root::NativeHandle<#marker> }
            },
        )
    });
    quote! {
        #[derive(::core::fmt::Debug)]
        #vis struct #owned_ident #generics #where_clause {
            #(#fields,)*
        }
    }
}

fn generate_into_owned(wire: &ItemStruct, handles: &[HandleField], root: &Path) -> TokenStream {
    let wire_ident = &wire.ident;
    let owned_ident = format_ident!("Owned{}", wire_ident);
    let (impl_generics, ty_generics, where_clause) = wire.generics.split_for_impl();
    let Fields::Named(named) = &wire.fields else {
        return quote! {};
    };
    let field_assigns = named.named.iter().enumerate().map(|(idx, f)| {
        let name = &f.ident;
        handles.iter().find(|h| h.index == idx).map_or_else(
            || quote! { #name: self.#name },
            |h| {
                let marker = &h.marker;
                quote! { #name: #root::NativeHandle::<#marker>::adopt(rt, self.#name) }
            },
        )
    });
    quote! {
        impl #impl_generics #wire_ident #ty_generics #where_clause {
            #[must_use]
            pub fn into_owned(
                self,
                rt: &::std::sync::Arc<#root::Runtime>,
            ) -> #owned_ident #ty_generics {
                #owned_ident {
                    #(#field_assigns,)*
                }
            }
        }
    }
}

struct MessageArgs {
    wincode_path: Path,
    crate_path: Path,
}

impl MessageArgs {
    /// The wincode derives, pointed at the wincode the caller can reach.
    fn derive(&self) -> TokenStream {
        let wincode = &self.wincode_path;
        let path = LitStr::new(
            &quote!(#wincode).to_string(),
            proc_macro2::Span::call_site(),
        );
        quote! {
            #[derive(#wincode::SchemaWrite, #wincode::SchemaRead)]
            #[wincode(crate = #path)]
        }
    }
}

impl syn::parse::Parse for MessageArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut wincode_path: Option<Path> = None;
        let mut crate_path: Option<Path> = None;
        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier"))?
                .to_string();
            match key.as_str() {
                "wincode" => wincode_path = Some(expect_path_lit(&pair.value, "wincode")?),
                "crate" => crate_path = Some(expect_path_lit(&pair.value, "crate")?),
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!("unknown argument `{other}`; expected `wincode` or `crate`"),
                    ));
                }
            }
        }
        let crate_path: Path = crate_path.unwrap_or_else(|| parse_quote!(::istmo));
        Ok(Self {
            wincode_path: wincode_path.unwrap_or_else(|| parse_quote!(#crate_path::wincode)),
            crate_path,
        })
    }
}
