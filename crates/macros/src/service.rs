//! `#[istmo::service]` attribute macro implementation.
//!
//! Expands a trait declaration with two async methods (`on_start` and
//! `on_stop`) into three siblings:
//!
//! * the trait itself, with the `async fn` signatures rewritten to
//!   `impl Future + Send + '_` so the adapter can spawn the future on an OS
//!   thread;
//! * a `<Trait>ServiceAdapter<Impl>` struct implementing
//!   [`istmo_core::Dispatch`] and [`istmo_core::Plugin`], keyed by the
//!   `name = "..."` argument;
//! * inherent `new(inner)` constructor on the adapter.
//!
//! The adapter reaches its owning runtime via
//! [`istmo_core::Dispatch::runtime_attached`] (called once at
//! `Runtime::register_host` time). When an inbound `on_start` frame arrives
//! it builds an [`istmo::plugins::ServiceContext`], spawns an OS thread that
//! drives the user's `on_start` future to completion, and stores the
//! [`istmo::plugins::StopNotifier`] under a `Mutex<Option<_>>`. An inbound
//! `on_stop` frame signals the notifier, joins the worker thread and awaits
//! the user's `on_stop` future.

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Expr, ExprLit, FnArg, ItemTrait, Lit, LitStr, MetaNameValue, Path, ReturnType, Token,
    TraitItem, TraitItemFn, parse2, parse_quote, parse_str,
};

#[allow(unreachable_pub, clippy::too_many_lines)]
pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: ServiceArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def)?;

    let (on_start_returns_result, on_stop_present) = classify_methods(&trait_def)?;

    // Snapshot untouched signatures before Send-rewrite so we can check
    // shapes above and later work off the desugared version.
    add_send_bound_to_async_methods(&mut trait_def);

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let core = &args.core_path;
    let plugins = &args.plugins_path;
    let adapter_ident = format_ident!("{}Adapter", trait_ident);
    let state_ident = format_ident!("{}AdapterState", trait_ident);

    let on_start_body = if on_start_returns_result {
        quote! {
            match #core::__private::block_on(inner.on_start(ctx)) {
                ::core::result::Result::Ok(()) => {}
                ::core::result::Result::Err(err) => {
                    #core::__private::tracing::error!(
                        error = ?err,
                        service = %service_id_for_thread,
                        "service on_start returned an error",
                    );
                }
            }
        }
    } else {
        quote! {
            #core::__private::block_on(inner.on_start(ctx));
        }
    };

    let on_stop_call = if on_stop_present {
        quote! { self.__inner.on_stop().await; }
    } else {
        quote! {}
    };

    Ok(quote! {
        #trait_def

        #[doc(hidden)]
        struct #state_ident {
            notifier: #plugins::StopNotifier,
            thread: ::core::option::Option<::std::thread::JoinHandle<()>>,
        }

        #vis struct #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            __inner: ::std::sync::Arc<Impl>,
            __runtime: ::std::sync::OnceLock<::std::sync::Weak<#core::Runtime>>,
            __state: ::std::sync::Mutex<::core::option::Option<#state_ident>>,
        }

        impl<Impl> #adapter_ident<Impl>
        where
            Impl: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static,
        {
            pub fn new(inner: Impl) -> Self {
                Self {
                    __inner: ::std::sync::Arc::new(inner),
                    __runtime: ::std::sync::OnceLock::new(),
                    __state: ::std::sync::Mutex::new(::core::option::Option::None),
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
                        "on_start" => {
                            let ((service_id,), _) = #core::codec::decode::<(::std::string::String,)>(payload)
                                .map_err(#core::DispatchError::Decode)?;
                            let runtime = self
                                .__runtime
                                .get()
                                .and_then(|w| w.upgrade())
                                .ok_or(#core::DispatchError::Unimplemented(
                                    "service adapter has no runtime; call register_host before on_start",
                                ))?;
                            let (notifier, stop_rx) = #plugins::stop_channel();
                            let ctx = #plugins::ServiceContext::new(
                                runtime,
                                service_id.clone(),
                                stop_rx,
                            );
                            let inner = ::std::sync::Arc::clone(&self.__inner);
                            let service_id_for_thread = service_id.clone();
                            let handle = ::std::thread::spawn(move || {
                                #on_start_body
                            });
                            {
                                let mut guard = self.__state.lock().unwrap_or_else(|p| p.into_inner());
                                if let ::core::option::Option::Some(mut prev) = guard.take() {
                                    prev.notifier.signal();
                                    if let ::core::option::Option::Some(h) = prev.thread.take() {
                                        let _ = h.join();
                                    }
                                }
                                *guard = ::core::option::Option::Some(#state_ident {
                                    notifier,
                                    thread: ::core::option::Option::Some(handle),
                                });
                            }
                            let bytes = #core::codec::encode(&())
                                .map_err(#core::DispatchError::Encode)?;
                            ::core::result::Result::Ok(#core::Outcome::Ok(bytes))
                        }
                        "on_stop" => {
                            let ((), _) = #core::codec::decode::<()>(payload)
                                .map_err(#core::DispatchError::Decode)?;
                            let state = {
                                let mut guard = self.__state.lock().unwrap_or_else(|p| p.into_inner());
                                guard.take()
                            };
                            if let ::core::option::Option::Some(mut state) = state {
                                state.notifier.signal();
                                if let ::core::option::Option::Some(h) = state.thread.take() {
                                    let _ = h.join();
                                }
                            }
                            #on_stop_call
                            let bytes = #core::codec::encode(&())
                                .map_err(#core::DispatchError::Encode)?;
                            ::core::result::Result::Ok(#core::Outcome::Ok(bytes))
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
            "generic service traits are not supported",
        ));
    }
    if !t.supertraits.is_empty() {
        return Err(syn::Error::new_spanned(
            &t.supertraits,
            "supertraits on a service trait are not supported",
        ));
    }
    Ok(())
}

/// Verify the trait has exactly the methods `#[istmo::service]` expects, and
/// return `(on_start_returns_result, on_stop_present)`.
fn classify_methods(t: &ItemTrait) -> syn::Result<(bool, bool)> {
    let mut on_start: Option<&TraitItemFn> = None;
    let mut on_stop: Option<&TraitItemFn> = None;
    for item in &t.items {
        let TraitItem::Fn(f) = item else {
            continue;
        };
        match f.sig.ident.to_string().as_str() {
            "on_start" => on_start = Some(f),
            "on_stop" => on_stop = Some(f),
            other => {
                return Err(syn::Error::new_spanned(
                    &f.sig.ident,
                    format!(
                        "unexpected method `{other}` on service trait; only `on_start` and `on_stop` are allowed"
                    ),
                ));
            }
        }
    }
    let on_start = on_start.ok_or_else(|| {
        syn::Error::new_spanned(
            &t.ident,
            "service trait must declare `async fn on_start(&self, ctx: ServiceContext) -> Result<(), _>`",
        )
    })?;
    if on_start.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &on_start.sig.ident,
            "`on_start` must be `async fn`",
        ));
    }
    // `&self, ctx: ServiceContext` — receiver + one typed arg.
    if on_start.sig.inputs.len() != 2
        || !matches!(on_start.sig.inputs.first(), Some(FnArg::Receiver(_)))
    {
        return Err(syn::Error::new_spanned(
            &on_start.sig,
            "`on_start` must take `&self` and a single `ctx: ServiceContext` argument",
        ));
    }
    let on_start_returns_result = matches!(
        &on_start.sig.output,
        ReturnType::Type(_, ty) if is_result_type(ty)
    );
    let on_stop_present = if let Some(f) = on_stop {
        if f.sig.asyncness.is_none() {
            return Err(syn::Error::new_spanned(
                &f.sig.ident,
                "`on_stop` must be `async fn`",
            ));
        }
        if f.sig.inputs.len() != 1
            || !matches!(f.sig.inputs.first(), Some(FnArg::Receiver(_)))
        {
            return Err(syn::Error::new_spanned(
                &f.sig,
                "`on_stop` must take only `&self`",
            ));
        }
        true
    } else {
        false
    };
    Ok((on_start_returns_result, on_stop_present))
}

fn is_result_type(ty: &syn::Type) -> bool {
    let syn::Type::Path(tp) = ty else {
        return false;
    };
    tp.path
        .segments
        .last()
        .is_some_and(|s| s.ident == "Result")
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

struct ServiceArgs {
    plugin_id: LitStr,
    core_path: Path,
    plugins_path: Path,
}

impl syn::parse::Parse for ServiceArgs {
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
                        syn::Error::new_spanned(
                            &pair.value,
                            format!("invalid `plugins` path: {e}"),
                        )
                    })?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &pair.path,
                        format!("unknown argument `{other}`; expected `name`, `crate` or `plugins`"),
                    ));
                }
            }
        }

        Ok(Self {
            plugin_id: plugin_id.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[istmo::service] requires a `name = \"...\"` argument",
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
