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
    let args: WorkerArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def, TraitKind::Worker)?;

    let run_returns_result = classify_run_method(&trait_def)?;

    desugar_async_methods(&mut trait_def);

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let core = &args.core_path;
    let plugins = &args.plugins_path;
    let adapter_ident = format_ident!("{}WorkerAdapter", trait_ident);
    let adapter_doc = format!(
        " Runs a [`{trait_ident}`] implementation as the `{}` worker: decodes each\n \
         scheduled task, awaits `run` and reports its outcome.",
        plugin_id.value(),
    );

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

        #[doc = #adapter_doc]
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
            /// Adapter running `inner`.
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
                cancel: #core::CancelToken,
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
                                cancel,
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
    if run.sig.inputs.len() != 2 {
        return Err(syn::Error::new_spanned(
            &run.sig,
            "`run` must take `&self` and a single `ctx: WorkerContext` argument",
        ));
    }
    Ok(matches!(
        classify_return(&run.sig.output)?,
        ReturnShape::Result { .. }
    ))
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
                    "#[istmo::worker] requires a `name = \"...\"` argument",
                )
            })?,
            plugins_path: plugins_path.unwrap_or_else(|| parse_quote!(#core_path::plugins)),
            core_path,
        })
    }
}
