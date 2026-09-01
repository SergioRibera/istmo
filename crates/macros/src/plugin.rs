//! `#[istmo::plugin]` attribute macro implementation.

use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, ExprPath, FnArg, GenericArgument, Ident, ItemTrait, Lit, LitStr,
    MetaNameValue, PathArguments, ReturnType, Token, TraitItem, TraitItemFn, Type, TypePath,
    parse2,
};

#[allow(unreachable_pub)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: PluginArgs = parse2(attr)?;
    let trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def)?;

    let type_name = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();

    let methods: Vec<&TraitItemFn> = trait_def
        .items
        .iter()
        .filter_map(|item| match item {
            TraitItem::Fn(f) => Some(f),
            _ => None,
        })
        .collect();

    let method_impls = methods
        .iter()
        .map(|m| expand_method(&plugin_id, m))
        .collect::<syn::Result<Vec<_>>>()?;

    let stateless_ctors = if args.init.is_none() {
        Some(expand_stateless_ctors(&type_name))
    } else {
        None
    };

    let stateful_ctors = args
        .init
        .as_ref()
        .map(|init_ty| expand_stateful_ctors(&type_name, &plugin_id, init_ty));

    let struct_def = quote! {
        #vis struct #type_name {
            __runtime: ::std::sync::Arc<::istmo::Runtime>,
            __instance_id: ::core::option::Option<::istmo::InstanceId>,
        }

        impl ::core::fmt::Debug for #type_name {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#type_name))
                    .field("instance_id", &self.__instance_id)
                    .finish_non_exhaustive()
            }
        }
    };

    let drop_impl = quote! {
        impl ::core::ops::Drop for #type_name {
            fn drop(&mut self) {
                if let ::core::option::Option::Some(id) = self.__instance_id {
                    let _ = self.__runtime.destroy_instance(id);
                }
            }
        }
    };

    Ok(quote! {
        #struct_def

        #stateless_ctors

        #stateful_ctors

        impl #type_name {
            #(#method_impls)*
        }

        #drop_impl
    })
}

fn reject_unsupported_trait_shape(t: &ItemTrait) -> syn::Result<()> {
    if !t.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.generics,
            "generic plugin traits are not supported yet",
        ));
    }
    if !t.supertraits.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.supertraits,
            "supertraits on a plugin trait are not supported",
        ));
    }
    Ok(())
}

fn expand_stateless_ctors(type_name: &Ident) -> TokenStream {
    quote! {
        impl #type_name {
            pub fn acquire() -> ::core::result::Result<Self, ::istmo::IstmoError> {
                ::core::result::Result::Ok(Self {
                    __runtime: ::istmo::Runtime::global()?,
                    __instance_id: ::core::option::Option::None,
                })
            }

            #[must_use]
            pub fn from_runtime(rt: &::std::sync::Arc<::istmo::Runtime>) -> Self {
                Self {
                    __runtime: ::std::sync::Arc::clone(rt),
                    __instance_id: ::core::option::Option::None,
                }
            }
        }
    }
}

fn expand_stateful_ctors(type_name: &Ident, plugin_id: &LitStr, init_ty: &Type) -> TokenStream {
    quote! {
        impl #type_name {
            pub async fn acquire_with(
                config: #init_ty,
            ) -> ::core::result::Result<Self, ::istmo::IstmoError> {
                let rt = ::istmo::Runtime::global()?;
                Self::from_runtime_with(&rt, config).await
            }

            pub async fn from_runtime_with(
                rt: &::std::sync::Arc<::istmo::Runtime>,
                config: #init_ty,
            ) -> ::core::result::Result<Self, ::istmo::IstmoError> {
                let payload = ::istmo::codec::encode(&config)?;
                let handle = rt.create_instance(#plugin_id, payload)?;
                let bytes = match handle.await? {
                    ::core::result::Result::Ok(b) => b,
                    ::core::result::Result::Err(b) => {
                        return ::core::result::Result::Err(
                            ::istmo::IstmoError::PluginError { bytes: b },
                        );
                    }
                };
                let (instance_id, _) =
                    ::istmo::codec::decode::<::istmo::InstanceId>(&bytes)?;
                rt.register_instance(instance_id, #plugin_id);
                ::core::result::Result::Ok(Self {
                    __runtime: ::std::sync::Arc::clone(rt),
                    __instance_id: ::core::option::Option::Some(instance_id),
                })
            }
        }
    }
}

struct PluginArgs {
    plugin_id: LitStr,
    init: Option<Type>,
}

impl syn::parse::Parse for PluginArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut plugin_id: Option<LitStr> = None;
        let mut init: Option<Type> = None;

        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier"))?
                .to_string();
            match key.as_str() {
                "name" => {
                    plugin_id = Some(expect_lit_str(&pair.value)?);
                }
                "init" => {
                    init = Some(expect_type(&pair.value)?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!("unknown argument `{other}`; expected `name` or `init`"),
                    ));
                }
            }
        }

        Ok(Self {
            plugin_id: plugin_id.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[istmo::plugin] requires a `name = \"...\"` argument",
                )
            })?,
            init,
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

fn expect_type(expr: &Expr) -> syn::Result<Type> {
    match expr {
        Expr::Path(ExprPath { path, qself, .. }) => Ok(Type::Path(TypePath {
            qself: qself.clone(),
            path: path.clone(),
        })),
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected a type path, e.g. `MyConfig`",
        )),
    }
}

fn expand_method(plugin_id: &LitStr, method: &TraitItemFn) -> syn::Result<TokenStream> {
    let sig = &method.sig;
    let name = &sig.ident;
    let method_name_str = LitStr::new(&name.to_string(), name.span());

    let (arg_pats, arg_names) = extract_args(sig)?;
    let payload_expr = build_payload_expr(&arg_names);

    let return_ty = match &sig.output {
        ReturnType::Default => Type::Verbatim(quote! { () }),
        ReturnType::Type(_, t) => (**t).clone(),
    };
    let (ok_ty, err_ty) = extract_result(&return_ty);

    if is_stream_method(&method.attrs) {
        Ok(expand_stream_method(
            plugin_id,
            name,
            &method_name_str,
            &arg_pats,
            &payload_expr,
            ok_ty.as_ref().unwrap_or(&return_ty),
            err_ty.as_ref(),
        ))
    } else {
        expand_unary_method(
            plugin_id,
            name,
            &method_name_str,
            sig.asyncness.is_some(),
            &arg_pats,
            &payload_expr,
            ok_ty.as_ref().unwrap_or(&return_ty),
        )
    }
}

fn extract_args(sig: &syn::Signature) -> syn::Result<(Vec<TokenStream>, Vec<Ident>)> {
    let mut pats = Vec::new();
    let mut names = Vec::new();
    for input in sig.inputs.iter().skip(1) {
        match input {
            FnArg::Typed(pat_type) => {
                let ident = match pat_type.pat.as_ref() {
                    syn::Pat::Ident(pi) => pi.ident.clone(),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &pat_type.pat,
                            "argument patterns must be plain identifiers",
                        ));
                    }
                };
                let ty = &pat_type.ty;
                pats.push(quote! { #ident: #ty });
                names.push(ident);
            }
            FnArg::Receiver(_) => {
                // The first input should be `&self`; if we see a receiver
                // past index 0 the trait is malformed. syn already rejects
                // multiple receivers, so this branch is just defensive.
            }
        }
    }
    Ok((pats, names))
}

fn build_payload_expr(arg_names: &[Ident]) -> TokenStream {
    if arg_names.is_empty() {
        quote! { ::istmo::codec::encode(&())? }
    } else {
        quote! { ::istmo::codec::encode(&( #(#arg_names,)* ))? }
    }
}

fn extract_result(ty: &Type) -> (Option<Type>, Option<Type>) {
    let Type::Path(tp) = ty else {
        return (None, None);
    };
    let Some(last) = tp.path.segments.last() else {
        return (None, None);
    };
    if last.ident != "Result" {
        return (None, None);
    }
    let PathArguments::AngleBracketed(args) = &last.arguments else {
        return (None, None);
    };
    let mut ok = None;
    let mut err = None;
    for arg in &args.args {
        if let GenericArgument::Type(t) = arg {
            if ok.is_none() {
                ok = Some(t.clone());
            } else if err.is_none() {
                err = Some(t.clone());
            }
        }
    }
    (ok, err)
}

fn is_stream_method(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| {
        let path = a.path();
        if path.is_ident("stream") {
            return true;
        }
        if path.segments.len() == 2 {
            let first = &path.segments[0].ident;
            let second = &path.segments[1].ident;
            return first == "istmo" && second == "stream";
        }
        false
    })
}

fn expand_unary_method(
    plugin_id: &LitStr,
    name: &Ident,
    method_name_str: &LitStr,
    is_async: bool,
    arg_pats: &[TokenStream],
    payload_expr: &TokenStream,
    ok_ty: &Type,
) -> syn::Result<TokenStream> {
    if !is_async {
        return Err(syn::Error::new_spanned(
            name,
            "unary plugin methods must be `async fn`",
        ));
    }
    Ok(quote! {
        pub async fn #name(&self, #(#arg_pats),*) -> ::core::result::Result<
            #ok_ty,
            ::istmo::IstmoError,
        > {
            let payload = #payload_expr;
            let handle = self.__runtime.call(
                #plugin_id,
                self.__instance_id,
                #method_name_str,
                payload,
            )?;
            let response = handle.await?;
            match response {
                ::core::result::Result::Ok(bytes) => {
                    let (value, _) = ::istmo::codec::decode::<#ok_ty>(&bytes)?;
                    ::core::result::Result::Ok(value)
                }
                ::core::result::Result::Err(bytes) => {
                    ::core::result::Result::Err(::istmo::IstmoError::PluginError { bytes })
                }
            }
        }
    })
}

fn expand_stream_method(
    plugin_id: &LitStr,
    name: &Ident,
    method_name_str: &LitStr,
    arg_pats: &[TokenStream],
    payload_expr: &TokenStream,
    item_ty: &Type,
    err_ty: Option<&Type>,
) -> TokenStream {
    let err_ty_tokens = err_ty.map_or_else(|| quote! { () }, |t| quote! { #t });
    quote! {
        pub fn #name(&self, #(#arg_pats),*) -> ::core::result::Result<
            ::istmo::TypedStream<#item_ty, #err_ty_tokens>,
            ::istmo::IstmoError,
        > {
            let payload = #payload_expr;
            let handle = self.__runtime.stream(
                #plugin_id,
                self.__instance_id,
                #method_name_str,
                payload,
                0,
            )?;
            ::core::result::Result::Ok(::istmo::TypedStream::new(handle))
        }
    }
}
