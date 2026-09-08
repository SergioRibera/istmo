//! `#[istmo::plugin]` attribute macro implementation.
//!
//! Expands to three siblings per annotated trait:
//! * the trait itself (kept verbatim),
//! * `<Trait>Client` — struct owning `Arc<Runtime>`, generated method
//!   wrappers, `Plugin` impl,
//! * `<Trait>Host<Impl: Trait + Send + Sync + 'static>` — dispatcher used by
//!   the `hosts:` side that implements `Dispatch` + `Plugin` and decodes
//!   inbound Call frames into direct calls on the concrete `Impl`.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Attribute, Expr, ExprLit, ExprPath, FnArg, GenericArgument, Ident, ItemTrait, Lit, LitStr,
    MetaNameValue, Path, PathArguments, ReturnType, Token, TraitItem, TraitItemFn, Type, TypePath,
    parse_quote, parse_str, parse2,
};

#[allow(unreachable_pub, clippy::too_many_lines)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: PluginArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def)?;

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let root = &args.crate_path;
    let client_ident = format_ident!("{}Client", trait_ident);
    let host_ident = format_ident!("{}Host", trait_ident);

    // Snapshot the original method signatures BEFORE we rewrite the trait for
    // Send-ness — client/host codegen needs the async-fn shape.
    let methods_source: Vec<TraitItemFn> = trait_def
        .items
        .iter()
        .filter_map(|item| match item {
            TraitItem::Fn(f) => Some(f.clone()),
            _ => None,
        })
        .collect();
    let methods: Vec<&TraitItemFn> = methods_source.iter().collect();

    // Rewrite the trait so every `async fn` returns an `impl Future + Send`;
    // native async-in-trait futures are not Send-by-default and the host
    // dispatcher needs to spawn them on a background thread.
    add_send_bound_to_async_methods(&mut trait_def);

    let client_methods = methods
        .iter()
        .map(|m| expand_client_method(&plugin_id, root, m))
        .collect::<syn::Result<Vec<_>>>()?;
    let host_arms = methods
        .iter()
        .map(|m| expand_host_arm(root, m))
        .collect::<syn::Result<Vec<_>>>()?;

    let stateless_ctors = if args.init.is_none() {
        Some(expand_stateless_ctors(&client_ident, root))
    } else {
        None
    };

    let stateful_ctors = args
        .init
        .as_ref()
        .map(|init_ty| expand_stateful_ctors(&client_ident, &plugin_id, init_ty, root));

    let client_struct = quote! {
        #vis struct #client_ident {
            __runtime: ::std::sync::Arc<#root::Runtime>,
            __instance_id: ::core::option::Option<#root::InstanceId>,
        }

        impl ::core::fmt::Debug for #client_ident {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#client_ident))
                    .field("instance_id", &self.__instance_id)
                    .finish_non_exhaustive()
            }
        }

        impl #root::Plugin for #client_ident {
            const PLUGIN_ID: &'static str = #plugin_id;
        }
    };

    let client_drop = quote! {
        impl ::core::ops::Drop for #client_ident {
            fn drop(&mut self) {
                if let ::core::option::Option::Some(id) = self.__instance_id {
                    let _ = self.__runtime.destroy_instance(id);
                }
            }
        }
    };

    let host_def = quote! {
        #vis struct #host_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            inner: Impl,
        }

        impl<Impl> #host_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            pub const fn new(inner: Impl) -> Self {
                Self { inner }
            }

            #[must_use]
            pub const fn inner(&self) -> &Impl {
                &self.inner
            }

            #[must_use]
            pub fn into_inner(self) -> Impl {
                self.inner
            }
        }

        impl<Impl> ::core::fmt::Debug for #host_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#host_ident)).finish_non_exhaustive()
            }
        }

        impl<Impl> #root::Plugin for #host_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            const PLUGIN_ID: &'static str = #plugin_id;
        }

        impl<Impl> #root::Dispatch for #host_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            fn plugin_id(&self) -> &'static str {
                #plugin_id
            }

            fn dispatch<'__istmo_a>(
                &'__istmo_a self,
                _instance_id: ::core::option::Option<#root::InstanceId>,
                method: &'__istmo_a str,
                payload: &'__istmo_a [u8],
                cancel: #root::CancelToken,
            ) -> #root::DispatchFuture<'__istmo_a> {
                ::std::boxed::Box::pin(async move {
                    match method {
                        #(#host_arms)*
                        other => ::core::result::Result::Err(
                            #root::DispatchError::UnknownMethod(other.to_owned())
                        ),
                    }
                })
            }
        }
    };

    Ok(quote! {
        #trait_def

        #client_struct

        #stateless_ctors

        #stateful_ctors

        impl #client_ident {
            #(#client_methods)*
        }

        #client_drop

        #host_def
    })
}

/// Desugar every `async fn foo(&self, ...) -> R` on the trait into
/// `fn foo(&self, ...) -> impl Future<Output = R> + Send + '_`.
///
/// Native async trait methods do not carry a `Send` bound on the returned
/// future, but the runtime spawns hosted dispatch tasks on a background
/// thread — so the future needs to be `Send`. Users still write plain
/// `async fn` in the trait declaration and in the impl block; the
/// implementation continues to satisfy the desugared signature because
/// `async fn` in an impl returns an anonymous `impl Future`.
fn add_send_bound_to_async_methods(trait_def: &mut ItemTrait) {
    for item in &mut trait_def.items {
        let TraitItem::Fn(f) = item else { continue };
        if f.sig.asyncness.is_none() {
            continue;
        }
        f.sig.asyncness = None;
        let return_ty = match &f.sig.output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, t) => quote! { #t },
        };
        let new_output: syn::Type = parse_quote! {
            impl ::core::future::Future<Output = #return_ty> + ::core::marker::Send + '_
        };
        f.sig.output = ReturnType::Type(<Token![->]>::default(), Box::new(new_output));
    }
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

fn expand_stateless_ctors(client_ident: &Ident, root: &Path) -> TokenStream {
    quote! {
        impl #client_ident {
            pub fn acquire() -> ::core::result::Result<Self, #root::IstmoError> {
                let rt = #root::Runtime::global()?;
                <Self as #root::Plugin>::PLUGIN_ID;
                rt.check_declared(<Self as #root::Plugin>::PLUGIN_ID)?;
                ::core::result::Result::Ok(Self {
                    __runtime: rt,
                    __instance_id: ::core::option::Option::None,
                })
            }

            pub fn from_runtime(
                rt: &::std::sync::Arc<#root::Runtime>,
            ) -> ::core::result::Result<Self, #root::IstmoError> {
                rt.check_declared(<Self as #root::Plugin>::PLUGIN_ID)?;
                ::core::result::Result::Ok(Self {
                    __runtime: ::std::sync::Arc::clone(rt),
                    __instance_id: ::core::option::Option::None,
                })
            }
        }
    }
}

fn expand_stateful_ctors(
    client_ident: &Ident,
    plugin_id: &LitStr,
    init_ty: &Type,
    root: &Path,
) -> TokenStream {
    quote! {
        impl #client_ident {
            pub async fn acquire_with(
                config: #init_ty,
            ) -> ::core::result::Result<Self, #root::IstmoError> {
                let rt = #root::Runtime::global()?;
                Self::from_runtime_with(&rt, config).await
            }

            pub async fn from_runtime_with(
                rt: &::std::sync::Arc<#root::Runtime>,
                config: #init_ty,
            ) -> ::core::result::Result<Self, #root::IstmoError> {
                rt.check_declared(<Self as #root::Plugin>::PLUGIN_ID)?;
                let payload = #root::codec::encode(&config)?;
                let handle = rt.create_instance(#plugin_id, payload)?;
                let bytes = match handle.await? {
                    ::core::result::Result::Ok(b) => b,
                    ::core::result::Result::Err(b) => {
                        return ::core::result::Result::Err(
                            #root::IstmoError::PluginError { bytes: b },
                        );
                    }
                };
                let (instance_id, _) =
                    #root::codec::decode::<#root::InstanceId>(&bytes)?;
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
    crate_path: Path,
}

impl syn::parse::Parse for PluginArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut plugin_id: Option<LitStr> = None;
        let mut init: Option<Type> = None;
        let mut crate_path: Option<Path> = None;

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
                "crate" => {
                    let path_str = expect_lit_str(&pair.value)?;
                    crate_path = Some(parse_str::<Path>(&path_str.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `crate` path: {e}"))
                    })?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!("unknown argument `{other}`; expected `name`, `init` or `crate`"),
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

/// Everything the per-method client expanders need beyond the return-type shape.
struct MethodCtx<'a> {
    plugin_id: &'a LitStr,
    root: &'a Path,
    name: &'a Ident,
    method_name_str: LitStr,
    arg_pats: Vec<TokenStream>,
    payload_expr: TokenStream,
}

/// One decoded trait method argument, tagged with whether it participates
/// in the wire payload or is filled from runtime state (currently only
/// `CancelToken`).
#[derive(Clone)]
struct WireArg {
    ident: Ident,
    ty: Type,
    role: ArgRole,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArgRole {
    Wire,
    Cancel,
}

fn arg_role(ty: &Type) -> ArgRole {
    if is_cancel_token(ty) {
        ArgRole::Cancel
    } else {
        ArgRole::Wire
    }
}

/// Loose match on the type's last path segment. Any type whose leaf name
/// is `CancelToken` counts — `CancelToken`, `istmo::CancelToken`,
/// `istmo_core::CancelToken`, etc. Aliasing to a differently-named type
/// (`type MyCancel = CancelToken;`) would break detection, but the trait
/// DSL is deliberately conservative here: the token is not `Encode` /
/// `Decode` anyway, so misclassifying it as a wire arg would fail
/// downstream loudly.
fn is_cancel_token(ty: &Type) -> bool {
    let Type::Path(tp) = ty else { return false };
    tp.path
        .segments
        .last()
        .is_some_and(|s| s.ident == "CancelToken")
}

fn expand_client_method(
    plugin_id: &LitStr,
    root: &Path,
    method: &TraitItemFn,
) -> syn::Result<TokenStream> {
    let sig = &method.sig;
    let name = &sig.ident;
    let method_name_str = LitStr::new(&name.to_string(), name.span());

    let all_args = extract_wire_args(sig)?;
    // Client omits Cancel args from its own signature — drop-cancel on
    // the returned future already fires `Frame::Cancel` for the caller.
    let wire_args: Vec<&WireArg> = all_args
        .iter()
        .filter(|a| a.role == ArgRole::Wire)
        .collect();
    let arg_pats: Vec<TokenStream> = wire_args
        .iter()
        .map(|a| {
            let n = &a.ident;
            let t = &a.ty;
            quote! { #n: #t }
        })
        .collect();
    let arg_names: Vec<Ident> = wire_args.iter().map(|a| a.ident.clone()).collect();
    let payload_expr = build_payload_expr(root, &arg_names);

    let return_ty = match &sig.output {
        ReturnType::Default => Type::Verbatim(quote! { () }),
        ReturnType::Type(_, t) => (**t).clone(),
    };
    let (ok_ty, err_ty) = extract_result(&return_ty);

    let ctx = MethodCtx {
        plugin_id,
        root,
        name,
        method_name_str,
        arg_pats,
        payload_expr,
    };

    if is_stream_method(&method.attrs) {
        Ok(expand_stream_method(
            &ctx,
            ok_ty.as_ref().unwrap_or(&return_ty),
            err_ty.as_ref(),
        ))
    } else {
        expand_unary_method(
            &ctx,
            sig.asyncness.is_some(),
            ok_ty.as_ref().unwrap_or(&return_ty),
        )
    }
}

fn expand_host_arm(root: &Path, method: &TraitItemFn) -> syn::Result<TokenStream> {
    let sig = &method.sig;
    let name = &sig.ident;
    let method_name_str = LitStr::new(&name.to_string(), name.span());

    let all_args = extract_wire_args(sig)?;
    let wire_arg_names: Vec<Ident> = all_args
        .iter()
        .filter(|a| a.role == ArgRole::Wire)
        .map(|a| a.ident.clone())
        .collect();
    let arg_tuple_type = wire_arg_tuple_type(&all_args);
    let decode = quote! {
        let (args, _) = #root::codec::decode::<#arg_tuple_type>(payload)
            .map_err(#root::DispatchError::Decode)?;
    };
    let destructure = destructure_arg_tuple(&wire_arg_names);
    // Pass wire args + the runtime-provided cancel token in the exact
    // slot the trait declares. Rebinding to the arg's own name lets us
    // interleave them with wire args by position.
    let cancel_bindings: Vec<TokenStream> = all_args
        .iter()
        .filter(|a| a.role == ArgRole::Cancel)
        .map(|a| {
            let n = &a.ident;
            quote! { let #n = cancel.clone(); }
        })
        .collect();
    let call_args: Vec<Ident> = all_args.iter().map(|a| a.ident.clone()).collect();

    if is_stream_method(&method.attrs) {
        // Hosted streams aren't supported yet — codegen leaves an
        // Unimplemented arm so the caller sees a typed error instead of a
        // silent unknown-method or a panic.
        let msg = format!("hosted stream method `{name}`");
        let literal = LitStr::new(&msg, Span::call_site());
        return Ok(quote! {
            #method_name_str => ::core::result::Result::Err(
                #root::DispatchError::Unimplemented(#literal)
            ),
        });
    }

    let return_ty = match &sig.output {
        ReturnType::Default => Type::Verbatim(quote! { () }),
        ReturnType::Type(_, t) => (**t).clone(),
    };
    let (ok_ty, err_ty) = extract_result(&return_ty);
    let call_expr = quote! {
        {
            #(#cancel_bindings)*
            self.inner.#name(#(#call_args),*).await
        }
    };

    let body = if let (Some(_), Some(_)) = (ok_ty.as_ref(), err_ty.as_ref()) {
        quote! {
            match #call_expr {
                ::core::result::Result::Ok(value) => {
                    let bytes = #root::codec::encode(&value)
                        .map_err(#root::DispatchError::Encode)?;
                    ::core::result::Result::Ok(#root::Outcome::Ok(bytes))
                }
                ::core::result::Result::Err(err) => {
                    let bytes = #root::codec::encode(&err)
                        .map_err(#root::DispatchError::Encode)?;
                    ::core::result::Result::Ok(#root::Outcome::DomainError(bytes))
                }
            }
        }
    } else {
        quote! {
            let value = #call_expr;
            let bytes = #root::codec::encode(&value)
                .map_err(#root::DispatchError::Encode)?;
            ::core::result::Result::Ok(#root::Outcome::Ok(bytes))
        }
    };

    Ok(quote! {
        #method_name_str => {
            #decode
            #destructure
            #body
        }
    })
}

fn wire_arg_tuple_type(args: &[WireArg]) -> TokenStream {
    let types: Vec<&Type> = args
        .iter()
        .filter(|a| a.role == ArgRole::Wire)
        .map(|a| &a.ty)
        .collect();
    if types.is_empty() {
        quote! { () }
    } else {
        quote! { ( #(#types,)* ) }
    }
}

fn destructure_arg_tuple(arg_names: &[Ident]) -> TokenStream {
    if arg_names.is_empty() {
        quote! { let _ = args; }
    } else {
        quote! { let ( #(#arg_names,)* ) = args; }
    }
}

fn extract_wire_args(sig: &syn::Signature) -> syn::Result<Vec<WireArg>> {
    let mut args = Vec::new();
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
                let ty = (*pat_type.ty).clone();
                let role = arg_role(&ty);
                args.push(WireArg { ident, ty, role });
            }
            FnArg::Receiver(_) => {
                // Defensive: syn rejects multiple receivers, so past index 0
                // this branch is unreachable in practice.
            }
        }
    }
    Ok(args)
}

fn build_payload_expr(root: &Path, arg_names: &[Ident]) -> TokenStream {
    if arg_names.is_empty() {
        quote! { #root::codec::encode(&())? }
    } else {
        quote! { #root::codec::encode(&( #(#arg_names,)* ))? }
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
    ctx: &MethodCtx<'_>,
    is_async: bool,
    ok_ty: &Type,
) -> syn::Result<TokenStream> {
    if !is_async {
        return Err(syn::Error::new_spanned(
            ctx.name,
            "unary plugin methods must be `async fn`",
        ));
    }
    let MethodCtx {
        plugin_id,
        root,
        name,
        method_name_str,
        arg_pats,
        payload_expr,
    } = ctx;
    Ok(quote! {
        pub async fn #name(&self, #(#arg_pats),*) -> ::core::result::Result<
            #ok_ty,
            #root::IstmoError,
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
                    let (value, _) = #root::codec::decode::<#ok_ty>(&bytes)?;
                    ::core::result::Result::Ok(value)
                }
                ::core::result::Result::Err(bytes) => {
                    ::core::result::Result::Err(#root::IstmoError::PluginError { bytes })
                }
            }
        }
    })
}

fn expand_stream_method(ctx: &MethodCtx<'_>, item_ty: &Type, err_ty: Option<&Type>) -> TokenStream {
    let MethodCtx {
        plugin_id,
        root,
        name,
        method_name_str,
        arg_pats,
        payload_expr,
    } = ctx;
    let err_ty_tokens = err_ty.map_or_else(|| quote! { () }, |t| quote! { #t });
    quote! {
        pub fn #name(&self, #(#arg_pats),*) -> ::core::result::Result<
            #root::TypedStream<#item_ty, #err_ty_tokens>,
            #root::IstmoError,
        > {
            let payload = #payload_expr;
            let handle = self.__runtime.stream(
                #plugin_id,
                self.__instance_id,
                #method_name_str,
                payload,
                0,
            )?;
            ::core::result::Result::Ok(#root::TypedStream::new(handle))
        }
    }
}
