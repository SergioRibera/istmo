use proc_macro2::TokenStream;
use quote::quote;
use syn::punctuated::Punctuated;
use syn::{
    Expr, Ident, Path, Token, bracketed, parse::Parse, parse::ParseStream, parse_str, parse2,
};

#[allow(unreachable_pub)]
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let RuntimeInput {
        plugins,
        hosts,
        services,
        workers,
        remote,
    } = parse2::<RuntimeInput>(input)?;

    let auto_plugin_paths = auto_paths_from_env("ISTMO_AUTO_PLUGINS")?;
    let auto_remote_paths = auto_paths_from_env("ISTMO_AUTO_REMOTE")?;
    let expects_calls = plugins
        .iter()
        .map(|client_ident| quote! { .expects::<#client_ident>() })
        .chain(
            auto_plugin_paths
                .into_iter()
                .map(|path| quote! { .expects::<#path>() }),
        );
    let remote_calls = remote
        .iter()
        .map(|trait_ident| {
            let client_ident = quote::format_ident!("{}Client", trait_ident);
            quote! { .remotes::<#client_ident>() }
        })
        .chain(
            auto_remote_paths
                .into_iter()
                .map(|path| quote! { .remotes::<#path>() }),
        );

    let host_calls = hosts.iter().map(
        |HostEntry {
             trait_ident,
             impl_expr,
         }| {
            let host_ident = quote::format_ident!("{}Host", trait_ident);
            quote! { .host(#host_ident::new(#impl_expr)) }
        },
    );

    let service_calls = services.iter().map(
        |HostEntry {
             trait_ident,
             impl_expr,
         }| {
            let adapter_ident = quote::format_ident!("{}Adapter", trait_ident);
            quote! { .host(#adapter_ident::new(#impl_expr)) }
        },
    );

    let worker_calls = workers.iter().map(
        |HostEntry {
             trait_ident,
             impl_expr,
         }| {
            let adapter_ident = quote::format_ident!("{}WorkerAdapter", trait_ident);
            quote! { .host(#adapter_ident::new(#impl_expr)) }
        },
    );

    Ok(quote! {
        #[cfg(target_os = "android")]
        #[allow(unused_imports)]
        pub use ::istmo::android::entrypoint::*;

        #[cfg(any(
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos",
        ))]
        #[allow(unused_imports)]
        pub use ::istmo::ios::entrypoint::*;

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub fn __istmo_configure_runtime(
            init: ::istmo::RuntimeInit,
        ) -> ::istmo::RuntimeInit {
            init
                #(#expects_calls)*
                #(#remote_calls)*
                #(#host_calls)*
                #(#service_calls)*
                #(#worker_calls)*
                .finish()
        }
    })
}

struct RuntimeInput {
    plugins: Vec<Ident>,
    hosts: Vec<HostEntry>,
    services: Vec<HostEntry>,
    workers: Vec<HostEntry>,
    remote: Vec<Ident>,
}

struct HostEntry {
    trait_ident: Ident,
    impl_expr: Expr,
}

impl Parse for RuntimeInput {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut plugins = Vec::new();
        let mut hosts = Vec::new();
        let mut services = Vec::new();
        let mut workers = Vec::new();
        let mut remote = Vec::new();

        while !input.is_empty() {
            let key: Ident = input.parse()?;
            input.parse::<Token![:]>()?;
            match key.to_string().as_str() {
                "plugins" => {
                    let content;
                    bracketed!(content in input);
                    let list: Punctuated<Ident, Token![,]> =
                        Punctuated::parse_terminated(&content)?;
                    plugins.extend(list);
                }
                "hosts" => {
                    let content;
                    bracketed!(content in input);
                    let list: Punctuated<HostEntry, Token![,]> =
                        Punctuated::parse_terminated(&content)?;
                    hosts.extend(list);
                }
                "services" => {
                    let content;
                    bracketed!(content in input);
                    let list: Punctuated<HostEntry, Token![,]> =
                        Punctuated::parse_terminated(&content)?;
                    services.extend(list);
                }
                "workers" => {
                    let content;
                    bracketed!(content in input);
                    let list: Punctuated<HostEntry, Token![,]> =
                        Punctuated::parse_terminated(&content)?;
                    workers.extend(list);
                }
                "remote" => {
                    let content;
                    bracketed!(content in input);
                    let list: Punctuated<Ident, Token![,]> =
                        Punctuated::parse_terminated(&content)?;
                    remote.extend(list);
                }
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown section `{other}`; expected `plugins`, `remote`, `hosts`, `services` or `workers`"
                        ),
                    ));
                }
            }

            let _: Option<Token![,]> = input.parse().ok();
        }

        Ok(Self {
            plugins,
            hosts,
            services,
            workers,
            remote,
        })
    }
}

impl Parse for HostEntry {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let trait_ident: Ident = input.parse()?;
        input.parse::<Token![=>]>()?;
        let impl_expr: Expr = input.parse()?;
        Ok(Self {
            trait_ident,
            impl_expr,
        })
    }
}

fn auto_paths_from_env(name: &str) -> syn::Result<Vec<Path>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(Vec::new());
    };
    let raw = raw.to_string_lossy();
    if raw.trim().is_empty() {
        return Ok(Vec::new());
    }
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| {
            parse_str::<Path>(s).map_err(|err| {
                syn::Error::new(
                    proc_macro2::Span::call_site(),
                    format!("istmo_build::emit_wiring_env produced an invalid path `{s}`: {err}"),
                )
            })
        })
        .collect()
}
