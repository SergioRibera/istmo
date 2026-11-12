use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::punctuated::Punctuated;
use syn::{
    Expr, Ident, Path, Token, bracketed, parse::Parse, parse::ParseStream, parse_str, parse2,
};

/// Env vars `istmo_build::emit_wiring_env` sets for the app crate.
const AUTO_PLUGINS_ENV: &str = "ISTMO_AUTO_PLUGINS";
const AUTO_REMOTE_ENV: &str = "ISTMO_AUTO_REMOTE";

pub(crate) fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let RuntimeInput {
        plugins,
        hosts,
        services,
        workers,
        remote,
    } = parse2::<RuntimeInput>(input)?;

    let auto_plugin_paths = auto_paths_from_env(AUTO_PLUGINS_ENV)?;
    let auto_remote_paths = auto_paths_from_env(AUTO_REMOTE_ENV)?;
    let expects_calls = plugins
        .into_iter()
        .chain(auto_plugin_paths)
        .map(|path| quote! { .expects::<#path>() });
    let remote_calls = remote
        .iter()
        .map(remote_client_path)
        .chain(auto_remote_paths)
        .map(|path| quote! { .remotes::<#path>() });

    let host_calls = hosts.iter().map(|entry| entry.register("Host"));
    let service_calls = services.iter().map(|entry| entry.register("Adapter"));
    let worker_calls = workers.iter().map(|entry| entry.register("WorkerAdapter"));

    let entrypoints = entrypoint_reexports();
    let app_info = app_info_initializer();

    Ok(quote! {
        #entrypoints

        // The auto-wiring lists are read when this macro expands; naming
        // them through `option_env!` records them in the crate's dep-info
        // so a changed list rebuilds the crate.
        const _: ::core::option::Option<&str> = ::core::option_env!(#AUTO_PLUGINS_ENV);
        const _: ::core::option::Option<&str> = ::core::option_env!(#AUTO_REMOTE_ENV);

        #app_info

        #[doc(hidden)]
        #[unsafe(no_mangle)]
        pub fn __istmo_configure_runtime(
            init: ::istmo::RuntimeInit,
        ) -> ::istmo::RuntimeInit {
            ::istmo::__private::set_app_info(
                ::core::option_env!("ISTMO_APP_BUNDLE_ID"),
                ::core::env!("CARGO_PKG_NAME"),
            );
            ::istmo::__private::set_app_assets(
                ::core::option_env!("ISTMO_APP_ASSETS_DIR"),
                ::core::env!("CARGO_MANIFEST_DIR"),
            );
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

fn entrypoint_reexports() -> TokenStream {
    quote! {
        #[cfg(target_os = "android")]
        #[allow(unused_imports, reason = "the platform loader resolves these symbols, not Rust code")]
        pub use ::istmo::android::entrypoint::*;

        #[cfg(any(
            target_os = "ios",
            target_os = "tvos",
            target_os = "watchos",
            target_os = "visionos",
        ))]
        #[allow(unused_imports, reason = "the platform loader resolves these symbols, not Rust code")]
        pub use ::istmo::ios::entrypoint::*;
    }
}

/// Forward the app crate's compile-time identity into istmo-core.
///
/// `env!` / `option_env!` expand at the app crate (where `emit_app` set
/// `ISTMO_APP_BUNDLE_ID` / `ISTMO_APP_ASSETS_DIR`) instead of
/// istmo-core's — otherwise desktop paths resolve under `istmo-core/`
/// and asset lookup falls back to istmo-core's own manifest dir.
///
/// The setters run twice: once from this static initializer so desktop
/// binaries that call `Runtime::init` directly (never touching
/// `__istmo_configure_runtime`) still get the correct paths, and once
/// from `__istmo_configure_runtime` so mobile bootstraps that skip the
/// static-initializer path on some odd linker configuration still win.
/// `OnceLock` de-duplicates transparently.
fn app_info_initializer() -> TokenStream {
    quote! {
        const _: () = {
            extern "C" fn __istmo_install_app_info() {
                ::istmo::__private::set_app_info(
                    ::core::option_env!("ISTMO_APP_BUNDLE_ID"),
                    ::core::env!("CARGO_PKG_NAME"),
                );
                ::istmo::__private::set_app_assets(
                    ::core::option_env!("ISTMO_APP_ASSETS_DIR"),
                    ::core::env!("CARGO_MANIFEST_DIR"),
                );
            }

            #[used]
            #[cfg_attr(
                any(target_os = "linux", target_os = "android"),
                unsafe(link_section = ".init_array")
            )]
            #[cfg_attr(
                any(
                    target_os = "macos",
                    target_os = "ios",
                    target_os = "tvos",
                    target_os = "watchos",
                    target_os = "visionos",
                ),
                unsafe(link_section = "__DATA,__mod_init_func")
            )]
            #[cfg_attr(target_os = "windows", unsafe(link_section = ".CRT$XCU"))]
            static __ISTMO_INIT_APP_INFO: extern "C" fn() = __istmo_install_app_info;
        };
    }
}

/// `remote:` entries name either the plugin trait (`Compute`, which
/// gets the `Client` suffix appended) or the client itself
/// (`ComputeClient`, `crate::api::ComputeClient`, used as-is).
fn remote_client_path(path: &Path) -> Path {
    if path
        .segments
        .last()
        .is_some_and(|s| s.ident.to_string().ends_with("Client"))
    {
        path.clone()
    } else {
        with_suffix(path, "Client")
    }
}

/// `path` with `suffix` appended to its last segment's name
/// (`api::Echo` + `Host` → `api::EchoHost`).
fn with_suffix(path: &Path, suffix: &str) -> Path {
    let mut out = path.clone();
    if let Some(last) = out.segments.last_mut() {
        last.ident = format_ident!("{}{}", last.ident, suffix, span = last.ident.span());
    }
    out
}

struct RuntimeInput {
    plugins: Vec<Path>,
    hosts: Vec<HostEntry>,
    services: Vec<HostEntry>,
    workers: Vec<HostEntry>,
    remote: Vec<Path>,
}

/// `Trait => impl_expr` inside `hosts:` / `services:` / `workers:`.
struct HostEntry {
    trait_path: Path,
    impl_expr: Expr,
}

impl HostEntry {
    /// `.host(<Trait><suffix>::new(impl_expr))`.
    fn register(&self, suffix: &str) -> TokenStream {
        let wrapper = with_suffix(&self.trait_path, suffix);
        let impl_expr = &self.impl_expr;
        quote! { .host(#wrapper::new(#impl_expr)) }
    }
}

fn parse_list<T: Parse>(input: ParseStream) -> syn::Result<Punctuated<T, Token![,]>> {
    let content;
    bracketed!(content in input);
    Punctuated::parse_terminated(&content)
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
                "plugins" => plugins.extend(parse_list::<Path>(input)?),
                "hosts" => hosts.extend(parse_list::<HostEntry>(input)?),
                "services" => services.extend(parse_list::<HostEntry>(input)?),
                "workers" => workers.extend(parse_list::<HostEntry>(input)?),
                "remote" => remote.extend(parse_list::<Path>(input)?),
                other => {
                    return Err(syn::Error::new(
                        key.span(),
                        format!(
                            "unknown section `{other}`; expected `plugins`, `remote`, `hosts`, `services` or `workers`"
                        ),
                    ));
                }
            }

            input.parse::<Option<Token![,]>>()?;
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
        let trait_path: Path = input.parse()?;
        input.parse::<Token![=>]>()?;
        let impl_expr: Expr = input.parse()?;
        Ok(Self {
            trait_path,
            impl_expr,
        })
    }
}

fn auto_paths_from_env(name: &str) -> syn::Result<Vec<Path>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(Vec::new());
    };
    let raw = raw.to_string_lossy();
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
