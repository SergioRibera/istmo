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
        reject_redact_attrs(
            &variant.fields,
            "`#[redact]` is not supported inside enums; carry the secret in a `#[message]` struct field",
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
        reject_redact_attrs(
            &wire.fields,
            "`#[redact]` requires a struct with named fields",
        )?;
    }
    let handles = collect_handle_fields(&wire)?;
    let redacted = collect_redacted_fields(&wire)?;
    if !redacted.is_empty() {
        reject_derived_debug(&wire)?;
    }
    strip_marker_attrs(&mut wire);

    let derive = args.derive();
    // A struct with secrets gets its `Debug` from the macro: a derived
    // one would print them.
    let wire_debug = if redacted.is_empty() {
        quote! {}
    } else {
        generate_debug(&wire, &wire.ident, &redacted)
    };
    let wire_tokens = quote! {
        #derive
        #wire
        #wire_debug
    };

    if handles.is_empty() {
        return Ok(wire_tokens);
    }

    let root = &args.crate_path;
    let owned = generate_owned_struct(&wire, &handles, &redacted, root);
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

/// `#[redact]` is consumed the same way as `#[handle]`.
fn reject_redact_attrs(fields: &Fields, message: &str) -> syn::Result<()> {
    fields
        .iter()
        .flat_map(|f| &f.attrs)
        .find(|a| is_marker_attr(a, "redact"))
        .map_or(Ok(()), |attr| Err(syn::Error::new_spanned(attr, message)))
}

/// Indices of the fields marked `#[redact]`.
fn collect_redacted_fields(s: &ItemStruct) -> syn::Result<Vec<usize>> {
    let Fields::Named(named) = &s.fields else {
        return Ok(Vec::new());
    };
    let mut out = Vec::new();
    for (index, f) in named.named.iter().enumerate() {
        let Some(attr) = f.attrs.iter().find(|a| is_marker_attr(a, "redact")) else {
            continue;
        };
        if !matches!(attr.meta, syn::Meta::Path(_)) {
            return Err(syn::Error::new_spanned(
                attr,
                "`#[redact]` takes no arguments",
            ));
        }
        out.push(index);
    }
    Ok(out)
}

/// The macro writes `Debug` for a struct with `#[redact]` fields, so a
/// derived one would collide with it (and print the secrets).
fn reject_derived_debug(s: &ItemStruct) -> syn::Result<()> {
    for attr in s.attrs.iter().filter(|a| a.path().is_ident("derive")) {
        let paths = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)?;
        if let Some(debug) = paths
            .iter()
            .find(|p| p.segments.last().is_some_and(|seg| seg.ident == "Debug"))
        {
            return Err(syn::Error::new_spanned(
                debug,
                "a struct with `#[redact]` fields gets its `Debug` impl from `#[istmo::message]`; remove `Debug` from the derive",
            ));
        }
    }
    Ok(())
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
    is_marker_attr(attr, "handle")
}

/// `#[marker]`, `#[istmo::marker]` or `#[istmo_macros::marker]`.
fn is_marker_attr(attr: &Attribute, marker: &str) -> bool {
    let segs: Vec<String> = attr
        .path()
        .segments
        .iter()
        .map(|s| s.ident.to_string())
        .collect();
    match segs.as_slice() {
        [only] => only == marker,
        [ns, name] => (ns == "istmo" || ns == "istmo_macros") && name == marker,
        _ => false,
    }
}

fn strip_marker_attrs(s: &mut ItemStruct) {
    if let Fields::Named(named) = &mut s.fields {
        for f in &mut named.named {
            f.attrs
                .retain(|a| !is_marker_attr(a, "handle") && !is_marker_attr(a, "redact"));
        }
    }
}

/// `Debug` for a named-field struct shaped like `wire` and named `ident`,
/// printing `<redacted>` for the fields at `redacted`. Type parameters
/// are bounded by `Debug`, as the derive does.
fn generate_debug(wire: &ItemStruct, ident: &syn::Ident, redacted: &[usize]) -> TokenStream {
    let Fields::Named(named) = &wire.fields else {
        return quote! {};
    };
    let mut generics = wire.generics.clone();
    for param in generics.type_params_mut() {
        param.bounds.push(parse_quote!(::core::fmt::Debug));
    }
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();
    let name = ident.to_string();
    let fields = named.named.iter().enumerate().map(|(idx, f)| {
        let field = &f.ident;
        let label = field.as_ref().map(ToString::to_string).unwrap_or_default();
        if redacted.contains(&idx) {
            quote! { .field(#label, &::core::format_args!("<redacted>")) }
        } else {
            quote! { .field(#label, &self.#field) }
        }
    });
    quote! {
        impl #impl_generics ::core::fmt::Debug for #ident #ty_generics #where_clause {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(#name)
                    #(#fields)*
                    .finish()
            }
        }
    }
}

fn generate_owned_struct(
    wire: &ItemStruct,
    handles: &[HandleField],
    redacted: &[usize],
    root: &Path,
) -> TokenStream {
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
        let docs: Vec<_> = f
            .attrs
            .iter()
            .filter(|a| a.path().is_ident("doc"))
            .collect();
        handles.iter().find(|h| h.index == idx).map_or_else(
            || {
                let ty = &f.ty;
                quote! { #(#docs)* #field_vis #name: #ty }
            },
            |h| {
                let marker = &h.marker;
                quote! { #(#docs)* #field_vis #name: #root::NativeHandle<#marker> }
            },
        )
    });
    // The wire type's field list, minus whatever `#[redact]` hides.
    let debug = generate_debug(wire, &owned_ident, redacted);
    let doc = format!(
        " [`{wire}`] with every `#[handle]` id adopted into a `NativeHandle` that\n \
         releases it on drop; see [`{wire}::into_owned`].",
        wire = wire.ident,
    );
    quote! {
        #[doc = #doc]
        #vis struct #owned_ident #generics #where_clause {
            #(#fields,)*
        }
        #debug
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
            /// Adopts every `#[handle]` id into a `NativeHandle` bound to `rt`.
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
