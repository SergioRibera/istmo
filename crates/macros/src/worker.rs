//! `#[istmo::worker]` attribute macro implementation.
//!
//! Emits three siblings per annotated trait:
//!
//! * the trait itself, with the `async fn run(...)` signature rewritten so
//!   the returned future is `Send + '_`;
//! * a `<Trait>WorkerAdapter<Impl>` struct implementing
//!   [`istmo_core::Dispatch`] and [`istmo_core::Plugin`], keyed by the
//!   `name = "..."` argument (which doubles as `WorkManager`'s unique task
//!   id on the wire);
//! * an inherent `new(inner)` constructor.
//!
//! Wire contract: the adapter answers the method name `"run"` with an
//! argument tuple `(String, String, Vec<u8>)` = `(task_id, unique_name,
//! input_bytes)`, and responds with the bincode-encoded `TaskOutcome`
//! returned by the impl (domain errors surface via
//! [`istmo_core::Outcome::DomainError`] as usual).

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Expr, ExprLit, FnArg, ItemTrait, Lit, LitStr, MetaNameValue, Path, ReturnType, Token,
    TraitItem, TraitItemFn, parse_quote, parse_str, parse2,
};

#[allow(unreachable_pub, clippy::too_many_lines)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: WorkerArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def)?;

    let run_returns_result = classify_run_method(&trait_def)?;

    add_send_bound_to_async_methods(&mut trait_def);

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let core = &args.core_path;
    let plugins = &args.plugins_path;
    let adapter_ident = format_ident!("{}WorkerAdapter", trait_ident);

    // Bind the outcome-encode block once so the two arms below stay short.
    let encode_outcome = quote! {
        {
            let bytes = #core::codec::encode(&outcome)
                .map_err(#core::DispatchError::Encode)?;
            ::core::result::Result::Ok(#core::Outcome::Ok(bytes))
        }
    };

    let run_dispatch_body = if run_returns_result {
        quote! {
            match self.__inner.run(ctx).await {
                ::core::result::Result::Ok(outcome) => #encode_outcome,
                ::core::result::Result::Err(err) => {
                    let bytes = #core::codec::encode(&err)
                        .map_err(#core::DispatchError::Encode)?;
                    ::core::result::Result::Ok(#core::Outcome::DomainError(bytes))
                }
            }
        }
    } else {
        quote! {
            let outcome = self.__inner.run(ctx).await;
            #encode_outcome
        }
    };

    Ok(quote! {
        #trait_def

        #vis struct #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            __inner: ::std::sync::Arc<Impl>,
            __runtime: ::std::sync::OnceLock<::std::sync::Weak<#core::Runtime>>,
        }

        impl<Impl> #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            pub fn new(inner: Impl) -> Self {
                Self {
                    __inner: ::std::sync::Arc::new(inner),
                    __runtime: ::std::sync::OnceLock::new(),
                }
            }
        }

        impl<Impl> ::core::fmt::Debug for #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#adapter_ident)).finish_non_exhaustive()
            }
        }

        impl<Impl> #core::Plugin for #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            const PLUGIN_ID: &'static str = #plugin_id;
        }

        impl<Impl> #core::Dispatch for #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            fn plugin_id(&self) -> &'static str {
                #plugin_id
            }

            fn runtime_attached(&self, runtime: ::std::sync::Weak<#core::Runtime>) {
                let _ = self.__runtime.set(runtime);
            }

            fn dispatch<'__istmo_a>(
                &'__istmo_a self,
                _instance_id: ::core::option::Option<#core::InstanceId>,
                method: &'__istmo_a str,
                payload: &'__istmo_a [u8],
            ) -> #core::DispatchFuture<'__istmo_a> {
                ::std::boxed::Box::pin(async move {
                    match method {
                        "run" => {
                            let ((task_id, unique_name, input), _) = #core::codec::decode::<(
                                ::std::string::String,
                                ::std::string::String,
                                ::std::vec::Vec<u8>,
                            )>(payload)
                                .map_err(#core::DispatchError::Decode)?;
                            let runtime = self
                                .__runtime
                                .get()
                                .and_then(|w| w.upgrade())
                                .ok_or(#core::DispatchError::Unimplemented(
                                    "worker adapter has no runtime; call register_host before run",
                                ))?;
                            let ctx = #plugins::WorkerContext::new(
                                runtime,
                                task_id,
                                unique_name,
                                input,
                            );
                            #run_dispatch_body
                        }
                        other => ::core::result::Result::Err(
                            #core::DispatchError::UnknownMethod(other.to_owned()),
                        ),
                    }
                })
            }
        }
    })
}

fn reject_unsupported_trait_shape(t: &ItemTrait) -> syn::Result<()> {
    if !t.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.generics,
            "generic worker traits are not supported",
        ));
    }
    if !t.supertraits.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.supertraits,
            "supertraits on a worker trait are not supported",
        ));
    }
    Ok(())
}

fn classify_run_method(t: &ItemTrait) -> syn::Result<bool> {
    let mut run: Option<&TraitItemFn> = None;
    for item in &t.items {
        let TraitItem::Fn(f) = item else {
            continue;
        };
        if f.sig.ident == "run" {
            run = Some(f);
        } else {
            return Err(syn::Error::new_spanned(
                &f.sig.ident,
                "worker trait may only declare `run`",
            ));
        }
    }
    let run = run.ok_or_else(|| {
        syn::Error::new_spanned(
            &t.ident,
            "worker trait must declare `async fn run(&self, ctx: WorkerContext) -> Result<TaskOutcome, _>`",
        )
    })?;
    if run.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &run.sig.ident,
            "`run` must be `async fn`",
        ));
    }
    if run.sig.inputs.len() != 2 || !matches!(run.sig.inputs.first(), Some(FnArg::Receiver(_))) {
        return Err(syn::Error::new_spanned(
            &run.sig,
            "`run` must take `&self` and a single `ctx: WorkerContext` argument",
        ));
    }
    let returns_result = matches!(
        &run.sig.output,
        ReturnType::Type(_, ty) if is_result_type(ty)
    );
    Ok(returns_result)
}

fn is_result_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    tp.path.segments.last().is_some_and(|s| s.ident == "Result")
}

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

struct WorkerArgs {
    plugin_id: LitStr,
    core_path: Path,
    plugins_path: Path,
}

impl syn::parse::Parse for WorkerArgs {
    fn parse(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let pairs = Punctuated::<MetaNameValue, Token![,]>::parse_terminated(input)?;
        let mut plugin_id: Option<LitStr> = None;
        let mut core_path: Option<Path> = None;
        let mut plugins_path: Option<Path> = None;

        for pair in pairs {
            let key = pair
                .path
                .get_ident()
                .ok_or_else(|| syn::Error::new_spanned(&pair.path, "expected identifier"))?
                .to_string();
            match key.as_str() {
                "name" => plugin_id = Some(expect_lit_str(&pair.value)?),
                "crate" => {
                    let path_str = expect_lit_str(&pair.value)?;
                    core_path = Some(parse_str::<Path>(&path_str.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `crate` path: {e}"))
                    })?);
                }
                "plugins" => {
                    let path_str = expect_lit_str(&pair.value)?;
                    plugins_path = Some(parse_str::<Path>(&path_str.value()).map_err(|e| {
                        syn::Error::new_spanned(&pair.value, format!("invalid `plugins` path: {e}"))
                    })?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!(
                            "unknown argument `{other}`; expected `name`, `crate` or `plugins`"
                        ),
                    ));
                }
            }
        }

        Ok(Self {
            plugin_id: plugin_id.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[istmo::worker] requires a `name = \"...\"` argument",
                )
            })?,
            core_path: core_path.unwrap_or_else(|| parse_quote!(::istmo)),
            plugins_path: plugins_path.unwrap_or_else(|| parse_quote!(::istmo::plugins)),
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
