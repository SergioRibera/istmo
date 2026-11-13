use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    ItemTrait, LitStr, MetaNameValue, Path, Token, TraitItem, TraitItemFn, parse_quote, parse2,
};

use crate::common::{
    ReturnShape, TraitKind, classify_return, desugar_async_methods, expect_lit_str,
    expect_path_lit, reject_unsupported_trait_shape,
};

#[expect(
    clippy::too_many_lines,
    reason = "one quote! block holding the whole adapter reads better than fragments stitched together"
)]
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: ServiceArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def, TraitKind::Service)?;

    let (on_start_returns_result, on_stop_present) = classify_methods(&trait_def)?;

    desugar_async_methods(&mut trait_def);

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let core = &args.core_path;
    let plugins = &args.plugins_path;
    let adapter_ident = format_ident!("{}Adapter", trait_ident);
    let adapter_doc = format!(
        " Runs a [`{trait_ident}`] implementation as the `{}` service: `on_start` on\n \
         its own thread, stopped by `on_stop`, a second start or a cancel.",
        plugin_id.value(),
    );
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
            cancel: #core::CancelToken,
            thread: ::std::thread::JoinHandle<()>,
        }

        impl #state_ident {
            /// Signal the running `on_start` to stop and wait for its
            /// thread. Must run without the adapter's state lock held:
            /// the join blocks for as long as `on_start` takes to wind
            /// down.
            fn shut_down(self) {
                self.notifier.signal();
                self.cancel.cancel();
                if self.thread.join().is_err() {
                    #core::__private::tracing::error!("service on_start thread panicked");
                }
            }
        }

        #[doc = #adapter_doc]
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
            /// Adapter running `inner`.
            pub fn new(inner: Impl) -> Self {
                Self {
                    __inner: ::std::sync::Arc::new(inner),
                    __runtime: ::std::sync::OnceLock::new(),
                    __state: ::std::sync::Mutex::new(::core::option::Option::None),
                }
            }

            fn __take_state(&self) -> ::core::option::Option<#state_ident> {
                self.__state.lock().unwrap_or_else(|p| p.into_inner()).take()
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
                cancel: #core::CancelToken,
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

                            // At most one `on_start` runs at a time: stop the
                            // previous one before starting the next.
                            if let ::core::option::Option::Some(previous) = self.__take_state() {
                                previous.shut_down();
                            }

                            let (notifier, stop_rx) = #plugins::stop_channel();
                            let ctx = #plugins::ServiceContext::new(
                                runtime,
                                service_id.clone(),
                                stop_rx,
                                cancel.clone(),
                            );

                            // Bridges a `Frame::Cancel` (which only trips the
                            // token) into the stop signal `ctx.stopped()` waits on.
                            let notifier_for_watch = notifier.clone();
                            let cancel_for_watch = cancel.clone();
                            ::std::thread::Builder::new()
                                .name(::std::string::String::from("istmo-svc-watch"))
                                .spawn(move || {
                                    #core::__private::block_on(cancel_for_watch.cancelled());
                                    notifier_for_watch.signal();
                                })
                                .map_err(|err| {
                                    #core::__private::tracing::error!(?err, "failed to spawn the service cancel watcher");
                                    #core::DispatchError::Spawn(err)
                                })?;

                            let inner = ::std::sync::Arc::clone(&self.__inner);
                            let service_id_for_thread = service_id.clone();
                            let spawned = ::std::thread::Builder::new()
                                .name(::std::string::String::from("istmo-service"))
                                .spawn(move || {
                                    #on_start_body
                                });
                            let thread = match spawned {
                                ::core::result::Result::Ok(thread) => thread,
                                ::core::result::Result::Err(err) => {
                                    // Releases the watcher thread.
                                    cancel.cancel();
                                    #core::__private::tracing::error!(?err, "failed to spawn the service thread");
                                    return ::core::result::Result::Err(#core::DispatchError::Spawn(err));
                                }
                            };

                            let raced = self
                                .__state
                                .lock()
                                .unwrap_or_else(|p| p.into_inner())
                                .replace(#state_ident {
                                    notifier,
                                    cancel: cancel.clone(),
                                    thread,
                                });
                            // A concurrent `on_start` stored its state after
                            // ours was taken above; the newest one wins.
                            if let ::core::option::Option::Some(raced) = raced {
                                raced.shut_down();
                            }

                            let bytes = #core::codec::encode(&())
                                .map_err(#core::DispatchError::Encode)?;
                            ::core::result::Result::Ok(#core::Outcome::Ok(bytes))
                        }
                        "on_stop" => {
                            let ((), _) = #core::codec::decode::<()>(payload)
                                .map_err(#core::DispatchError::Decode)?;
                            if let ::core::option::Option::Some(state) = self.__take_state() {
                                state.shut_down();
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
    if on_start.sig.inputs.len() != 2 {
        return Err(syn::Error::new_spanned(
            &on_start.sig,
            "`on_start` must take `&self` and a single `ctx: ServiceContext` argument",
        ));
    }
    let on_start_returns_result = matches!(
        classify_return(&on_start.sig.output)?,
        ReturnShape::Result { .. }
    );
    let on_stop_present = if let Some(f) = on_stop {
        if f.sig.asyncness.is_none() {
            return Err(syn::Error::new_spanned(
                &f.sig.ident,
                "`on_stop` must be `async fn`",
            ));
        }
        if f.sig.inputs.len() != 1 {
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
                "crate" => core_path = Some(expect_path_lit(&pair.value, "crate")?),
                "plugins" => plugins_path = Some(expect_path_lit(&pair.value, "plugins")?),
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

        let core_path = core_path.unwrap_or_else(|| parse_quote!(::istmo));
        Ok(Self {
            plugin_id: plugin_id.ok_or_else(|| {
                syn::Error::new(
                    input.span(),
                    "#[istmo::service] requires a `name = \"...\"` argument",
                )
            })?,
            plugins_path: plugins_path.unwrap_or_else(|| parse_quote!(#core_path::plugins)),
            core_path,
        })
    }
}
