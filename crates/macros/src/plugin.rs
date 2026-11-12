use proc_macro2::TokenStream;
use quote::{format_ident, quote, quote_spanned};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, ExprPath, FnArg, GenericArgument, Ident, ItemTrait, LitStr, MetaNameValue,
    Path, PathArguments, ReturnType, Token, TraitItem, TraitItemFn, Type, TypePath, parse_quote,
    parse2,
};

use crate::common::{
    ReturnShape, TraitKind, classify_return, desugar_async_methods, expect_lit_str,
    expect_path_lit, reject_unsupported_trait_shape,
};

/// Capacity of the hop between a hosted stream's encoder thread and the
/// runtime pump. Bounded so a slow consumer pushes back on the producer
/// instead of buffering encoded items without limit.
const STREAM_ENCODER_CAPACITY: usize = 64;

#[expect(
    clippy::too_many_lines,
    reason = "assembles the three emitted siblings; each piece is already its own function"
)]
pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let args: PluginArgs = parse2(attr)?;
    let mut trait_def: ItemTrait = parse2(item)?;

    reject_unsupported_trait_shape(&trait_def, TraitKind::Plugin)?;

    let trait_ident = trait_def.ident.clone();
    let vis = trait_def.vis.clone();
    let plugin_id = args.plugin_id.clone();
    let root = &args.crate_path;
    let client_ident = format_ident!("{}Client", trait_ident);
    let host_ident = format_ident!("{}Host", trait_ident);

    let specs = trait_def
        .items
        .iter()
        .filter_map(|item| match item {
            TraitItem::Fn(f) => Some(MethodSpec::parse(f)),
            _ => None,
        })
        .collect::<syn::Result<Vec<_>>>()?;

    rewrite_trait_signatures(&mut trait_def, root);

    let client_methods: Vec<TokenStream> = specs
        .iter()
        .map(|spec| expand_client_method(&plugin_id, root, spec))
        .collect::<syn::Result<_>>()?;
    let host_arms: Vec<TokenStream> = specs
        .iter()
        .map(|spec| expand_host_arm(root, spec))
        .collect();
    let error_bounds = expand_error_bounds(&specs);
    let message_bounds = expand_message_bounds(root, &specs);

    let ctors = args.init.as_ref().map_or_else(
        || expand_stateless_ctors(&client_ident, root),
        |init_ty| expand_stateful_ctors(&client_ident, &plugin_id, init_ty, root),
    );

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

    let factory_ident = format_ident!("{}Factory", trait_ident);
    let host_def = args.init.as_ref().map_or_else(
        || {
            expand_stateless_host(
                &vis,
                &trait_ident,
                &host_ident,
                &plugin_id,
                root,
                &host_arms,
            )
        },
        |init_ty| {
            expand_stateful_host(
                &vis,
                &trait_ident,
                &factory_ident,
                &host_ident,
                init_ty,
                &plugin_id,
                root,
                &host_arms,
            )
        },
    );

    Ok(quote! {
        #trait_def

        #error_bounds

        #message_bounds

        #client_struct

        #ctors

        impl #client_ident {
            #[must_use]
            pub const fn runtime(&self) -> &::std::sync::Arc<#root::Runtime> {
                &self.__runtime
            }

            #[must_use]
            pub const fn instance_id(&self) -> ::core::option::Option<#root::InstanceId> {
                self.__instance_id
            }

            #(#client_methods)*
        }

        #client_drop

        #host_def
    })
}

fn expand_stateless_host(
    vis: &syn::Visibility,
    trait_ident: &Ident,
    host_ident: &Ident,
    plugin_id: &LitStr,
    root: &Path,
    host_arms: &[TokenStream],
) -> TokenStream {
    quote! {
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
                let __istmo_impl = &self.inner;
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
    }
}

#[expect(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "one quote! block holding the whole host; each argument is a distinct piece of it"
)]
fn expand_stateful_host(
    vis: &syn::Visibility,
    trait_ident: &Ident,
    factory_ident: &Ident,
    host_ident: &Ident,
    init_ty: &Type,
    plugin_id: &LitStr,
    root: &Path,
    host_arms: &[TokenStream],
) -> TokenStream {
    quote! {
        /// Factory that constructs a per-instance implementation of the
        /// plugin trait from the wire-decoded configuration payload.
        #vis trait #factory_ident: ::core::marker::Send + ::core::marker::Sync + 'static {
            /// Concrete per-instance trait implementation this factory
            /// yields. The associated type keeps the plugin dispatcher
            /// free of trait objects, which the plugin trait's
            /// `impl Future` return types would otherwise disallow.
            type Instance: #trait_ident + ::core::marker::Send + ::core::marker::Sync + 'static;

            /// Build a new instance for a `CreateInstance` frame.
            fn create(&self, config: #init_ty) -> Self::Instance;
        }

        #vis struct #host_ident<F>
        where
            F: #factory_ident,
        {
            factory: F,
            instances: ::std::sync::Mutex<
                ::std::collections::HashMap<
                    #root::InstanceId,
                    ::std::sync::Arc<<F as #factory_ident>::Instance>,
                >,
            >,
            next_instance: ::std::sync::atomic::AtomicU64,
        }

        impl<F> #host_ident<F>
        where
            F: #factory_ident,
        {
            pub fn new(factory: F) -> Self {
                Self {
                    factory,
                    instances: ::std::sync::Mutex::new(::std::collections::HashMap::new()),
                    next_instance: ::std::sync::atomic::AtomicU64::new(1),
                }
            }

            #[must_use]
            pub const fn factory(&self) -> &F {
                &self.factory
            }
        }

        impl<F> ::core::fmt::Debug for #host_ident<F>
        where
            F: #factory_ident,
        {
            fn fmt(&self, f: &mut ::core::fmt::Formatter<'_>) -> ::core::fmt::Result {
                f.debug_struct(stringify!(#host_ident)).finish_non_exhaustive()
            }
        }

        impl<F> #root::Plugin for #host_ident<F>
        where
            F: #factory_ident,
        {
            const PLUGIN_ID: &'static str = #plugin_id;
        }

        impl<F> #root::Dispatch for #host_ident<F>
        where
            F: #factory_ident,
        {
            fn plugin_id(&self) -> &'static str {
                #plugin_id
            }

            fn create_instance<'__istmo_a>(
                &'__istmo_a self,
                config_payload: &'__istmo_a [u8],
                _cancel: #root::CancelToken,
            ) -> #root::DispatchFuture<'__istmo_a> {
                ::std::boxed::Box::pin(async move {
                    let (config, _) = #root::codec::decode::<#init_ty>(config_payload)
                        .map_err(#root::DispatchError::Decode)?;
                    let instance = ::std::sync::Arc::new(self.factory.create(config));
                    let id = #root::InstanceId::new(
                        self.next_instance
                            .fetch_add(1, ::std::sync::atomic::Ordering::Relaxed),
                    );
                    {
                        let mut guard = match self.instances.lock() {
                            ::core::result::Result::Ok(g) => g,
                            ::core::result::Result::Err(p) => p.into_inner(),
                        };
                        guard.insert(id, instance);
                    }
                    let bytes = #root::codec::encode(&id)
                        .map_err(#root::DispatchError::Encode)?;
                    ::core::result::Result::Ok(#root::Outcome::Ok(bytes))
                })
            }

            fn destroy_instance(&self, instance_id: #root::InstanceId) {
                let mut guard = match self.instances.lock() {
                    ::core::result::Result::Ok(g) => g,
                    ::core::result::Result::Err(p) => p.into_inner(),
                };
                guard.remove(&instance_id);
            }

            fn dispatch<'__istmo_a>(
                &'__istmo_a self,
                instance_id: ::core::option::Option<#root::InstanceId>,
                method: &'__istmo_a str,
                payload: &'__istmo_a [u8],
                cancel: #root::CancelToken,
            ) -> #root::DispatchFuture<'__istmo_a> {
                let iid = instance_id;
                let lookup = {
                    let guard = match self.instances.lock() {
                        ::core::result::Result::Ok(g) => g,
                        ::core::result::Result::Err(p) => p.into_inner(),
                    };
                    iid.and_then(|id| guard.get(&id).map(::std::sync::Arc::clone))
                };
                ::std::boxed::Box::pin(async move {
                    let __istmo_impl = lookup.ok_or_else(|| {
                        #root::DispatchError::UnknownMethod(
                            ::std::format!(
                                "{} requires a known instance id (got {:?})",
                                method,
                                iid,
                            ),
                        )
                    })?;
                    match method {
                        #(#host_arms)*
                        other => ::core::result::Result::Err(
                            #root::DispatchError::UnknownMethod(other.to_owned())
                        ),
                    }
                })
            }
        }
    }
}

/// Stream methods return a receiver of their declared item type; every
/// other method is an `async fn` desugared to a `Send` future.
fn rewrite_trait_signatures(trait_def: &mut ItemTrait, root: &Path) {
    for item in &mut trait_def.items {
        let TraitItem::Fn(f) = item else { continue };
        if !is_stream_method(&f.attrs) {
            continue;
        }
        let item_ty = match &f.sig.output {
            ReturnType::Default => quote! { () },
            ReturnType::Type(_, t) => quote! { #t },
        };
        let new_output: Type = parse_quote! {
            #root::__private::flume::Receiver<#item_ty>
        };
        f.sig.output = ReturnType::Type(<Token![->]>::default(), Box::new(new_output));
    }
    desugar_async_methods(trait_def);
}

fn expand_stateless_ctors(client_ident: &Ident, root: &Path) -> TokenStream {
    quote! {
        impl #client_ident {
            pub fn acquire() -> ::core::result::Result<Self, #root::IstmoError> {
                let rt = #root::Runtime::global()?;
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
                "crate" => crate_path = Some(expect_path_lit(&pair.value, "crate")?),
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

/// One trait method, parsed and validated once so the client and host
/// halves are generated from the same reading of its signature.
struct MethodSpec {
    name: Ident,
    method_name_str: LitStr,
    args: Vec<WireArg>,
    ret: ReturnShape,
    kind: MethodKind,
    doc_attrs: Vec<Attribute>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MethodKind {
    /// `async fn` answered with one response.
    Unary { owned: bool },
    /// `#[stream] fn` answered with a stream of events.
    Stream,
}

impl MethodSpec {
    fn parse(method: &TraitItemFn) -> syn::Result<Self> {
        let sig = &method.sig;
        let stream = is_stream_method(&method.attrs);
        let owned = is_owned_method(&method.attrs);
        let ret = classify_return(&sig.output)?;

        let kind = if stream {
            if let Some(asyncness) = &sig.asyncness {
                return Err(syn::Error::new_spanned(
                    asyncness,
                    "`#[istmo::stream]` methods are not `async`: drop `async` and \
                     declare the stream item as the return type",
                ));
            }
            if owned {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "`#[istmo::owned]` is not supported on stream methods yet",
                ));
            }
            if let ReturnShape::Result { .. } = &ret {
                return Err(syn::Error::new_spanned(
                    &sig.output,
                    "`#[istmo::stream]` items cannot be `Result<T, E>`: hosted streams only \
                     end with `StreamEnd::Complete`, so an error would travel as an item; \
                     use a `#[message]` enum item that carries the failure",
                ));
            }
            MethodKind::Stream
        } else {
            if sig.asyncness.is_none() {
                return Err(syn::Error::new_spanned(
                    &sig.ident,
                    "unary plugin methods must be `async fn`; mark stream methods with `#[istmo::stream]`",
                ));
            }
            MethodKind::Unary { owned }
        };

        Ok(Self {
            name: sig.ident.clone(),
            method_name_str: LitStr::new(&sig.ident.to_string(), sig.ident.span()),
            args: extract_wire_args(sig)?,
            ret,
            kind,
            doc_attrs: method
                .attrs
                .iter()
                .filter(|a| a.path().is_ident("doc"))
                .cloned()
                .collect(),
        })
    }

    fn wire_args(&self) -> impl Iterator<Item = &WireArg> {
        self.args.iter().filter(|a| a.role == ArgRole::Wire)
    }

    fn wire_arg_names(&self) -> Vec<&Ident> {
        self.wire_args().map(|a| &a.ident).collect()
    }

    /// `name: Type` for every wire argument, as written in the client.
    fn client_params(&self) -> Vec<TokenStream> {
        self.wire_args()
            .map(|a| {
                let n = &a.ident;
                let t = &a.ty;
                quote! { #n: #t }
            })
            .collect()
    }

    /// Every type that crosses the wire for this method.
    fn wire_types(&self) -> impl Iterator<Item = &Type> {
        self.wire_args()
            .map(|a| &a.ty)
            .chain(std::iter::once(self.ret.ok_ty()))
            .chain(self.ret.err_ty())
    }
}

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

/// Detection is by the last path segment's name, so `CancelToken`,
/// `istmo::CancelToken` and `::istmo_core::CancelToken` all match. An
/// alias under another name, `&CancelToken` or `Option<CancelToken>` do
/// not, and are treated as wire arguments.
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
    spec: &MethodSpec,
) -> syn::Result<TokenStream> {
    match spec.kind {
        MethodKind::Stream => Ok(expand_stream_method(plugin_id, root, spec)),
        MethodKind::Unary { owned } => {
            let base = expand_unary_method(plugin_id, root, spec);
            if owned {
                let owned = expand_unary_owned_method(root, spec)?;
                Ok(quote! { #base #owned })
            } else {
                Ok(base)
            }
        }
    }
}

fn expand_host_arm(root: &Path, spec: &MethodSpec) -> TokenStream {
    let MethodSpec {
        name,
        method_name_str,
        args,
        ..
    } = spec;
    let arg_tuple_type = wire_arg_tuple_type(spec);
    let decode = quote! {
        let (args, _) = #root::codec::decode::<#arg_tuple_type>(payload)
            .map_err(#root::DispatchError::Decode)?;
    };
    let destructure = destructure_arg_tuple(&spec.wire_arg_names());

    let cancel_bindings: Vec<TokenStream> = args
        .iter()
        .filter(|a| a.role == ArgRole::Cancel)
        .map(|a| {
            let n = &a.ident;
            quote! { let #n = cancel.clone(); }
        })
        .collect();
    let call_args: Vec<&Ident> = args.iter().map(|a| &a.ident).collect();

    let call = quote! {
        {
            #(#cancel_bindings)*
            __istmo_impl.#name(#(#call_args),*)
        }
    };

    let body = match (spec.kind, &spec.ret) {
        (MethodKind::Stream, ret) => {
            expand_stream_arm_body(root, method_name_str, ret.ok_ty(), &call)
        }
        (MethodKind::Unary { .. }, ReturnShape::Result { ok, err }) => {
            let encode_ok = quote_spanned! {ok.span()=> #root::codec::encode(&value) };
            let encode_err = quote_spanned! {err.span()=> #root::codec::encode(&err) };
            quote! {
                match #call.await {
                    ::core::result::Result::Ok(value) => {
                        let bytes = #encode_ok.map_err(#root::DispatchError::Encode)?;
                        ::core::result::Result::Ok(#root::Outcome::Ok(bytes))
                    }
                    ::core::result::Result::Err(err) => {
                        let bytes = #encode_err.map_err(#root::DispatchError::Encode)?;
                        ::core::result::Result::Ok(#root::Outcome::DomainError(bytes))
                    }
                }
            }
        }
        (MethodKind::Unary { .. }, ReturnShape::Plain(ty)) => {
            let encode = quote_spanned! {ty.span()=> #root::codec::encode(&value) };
            quote! {
                let value = #call.await;
                let bytes = #encode.map_err(#root::DispatchError::Encode)?;
                ::core::result::Result::Ok(#root::Outcome::Ok(bytes))
            }
        }
    };

    quote! {
        #method_name_str => {
            #decode
            #destructure
            #body
        }
    }
}

/// Host side of a stream method: a named encoder thread turns the
/// implementation's typed receiver into the byte receiver the runtime
/// pumps out as `Event` frames.
fn expand_stream_arm_body(
    root: &Path,
    method_name_str: &LitStr,
    item_ty: &Type,
    call: &TokenStream,
) -> TokenStream {
    let encode = quote_spanned! {item_ty.span()=> #root::codec::encode(&item) };
    quote! {
        let __istmo_source: #root::__private::flume::Receiver<#item_ty> = #call;
        let (__istmo_etx, __istmo_erx) =
            #root::__private::flume::bounded::<::std::vec::Vec<u8>>(#STREAM_ENCODER_CAPACITY);
        let __istmo_spawned = ::std::thread::Builder::new()
            .name(::std::string::String::from("istmo-stream-enc"))
            .spawn(move || {
                while let ::core::result::Result::Ok(item) = __istmo_source.recv() {
                    match #encode {
                        ::core::result::Result::Ok(bytes) => {
                            if __istmo_etx.send(bytes).is_err() {
                                break;
                            }
                        }
                        ::core::result::Result::Err(err) => {
                            #root::__private::tracing::error!(
                                ?err,
                                method = #method_name_str,
                                "hosted stream encode failed",
                            );
                            break;
                        }
                    }
                }
            });
        if let ::core::result::Result::Err(err) = __istmo_spawned {
            #root::__private::tracing::error!(
                ?err,
                method = #method_name_str,
                "failed to spawn the hosted stream encoder thread",
            );
            return ::core::result::Result::Err(#root::DispatchError::Unimplemented(
                "hosted stream encoder thread could not be spawned",
            ));
        }
        ::core::result::Result::Ok(#root::Outcome::StreamOpened(__istmo_erx))
    }
}

fn wire_arg_tuple_type(spec: &MethodSpec) -> TokenStream {
    let types: Vec<&Type> = spec.wire_args().map(|a| &a.ty).collect();
    if types.is_empty() {
        quote! { () }
    } else {
        quote! { ( #(#types,)* ) }
    }
}

fn destructure_arg_tuple(arg_names: &[&Ident]) -> TokenStream {
    if arg_names.is_empty() {
        quote! { let _ = args; }
    } else {
        quote! { let ( #(#arg_names,)* ) = args; }
    }
}

fn extract_wire_args(sig: &syn::Signature) -> syn::Result<Vec<WireArg>> {
    let mut args = Vec::new();
    for input in &sig.inputs {
        let FnArg::Typed(pat_type) = input else {
            // The receiver; `reject_unsupported_trait_shape` already
            // required it to be `&self`.
            continue;
        };
        let syn::Pat::Ident(pi) = pat_type.pat.as_ref() else {
            return Err(syn::Error::new_spanned(
                &pat_type.pat,
                "argument patterns must be plain identifiers",
            ));
        };
        if pi.by_ref.is_some() || pi.subpat.is_some() {
            return Err(syn::Error::new_spanned(
                &pat_type.pat,
                "argument patterns must be plain identifiers",
            ));
        }
        let ty = (*pat_type.ty).clone();
        let role = arg_role(&ty);
        args.push(WireArg {
            ident: pi.ident.clone(),
            ty,
            role,
        });
    }
    Ok(args)
}

fn build_payload_expr(root: &Path, spec: &MethodSpec) -> TokenStream {
    let names = spec.wire_arg_names();
    if names.is_empty() {
        quote! { #root::codec::encode(&())? }
    } else {
        quote! { #root::codec::encode(&( #(#names,)* ))? }
    }
}

fn expand_error_bounds(specs: &[MethodSpec]) -> TokenStream {
    let mut seen = std::collections::BTreeSet::new();
    let mut asserts = Vec::new();
    for err_ty in specs.iter().filter_map(|s| s.ret.err_ty()) {
        if is_unit(err_ty) {
            continue;
        }
        let key = quote! { #err_ty }.to_string();
        if !seen.insert(key) {
            continue;
        }
        asserts.push(quote_spanned! {err_ty.span()=>
            const _: fn() = || {
                fn __istmo_assert_error<T>()
                where
                    T: ::std::error::Error + 'static,
                {
                }
                __istmo_assert_error::<#err_ty>();
            };
        });
    }
    quote! { #(#asserts)* }
}

/// Assert, at the type's own span, that every wire type encodes, so a
/// missing `#[message]` points at the offending argument or return type
/// instead of at the macro invocation. Only `Encode` is checked: it is a
/// single bound, while `Decode` would repeat the error once per decode
/// size rung.
fn expand_message_bounds(root: &Path, specs: &[MethodSpec]) -> TokenStream {
    let mut seen = std::collections::BTreeSet::new();
    let mut asserts = Vec::new();
    for ty in specs.iter().flat_map(MethodSpec::wire_types) {
        if !seen.insert(quote! { #ty }.to_string()) {
            continue;
        }
        let call = quote_spanned! {ty.span()=> __istmo_assert_message::<#ty>() };
        asserts.push(quote! {
            const _: fn() = || {
                fn __istmo_assert_message<T: #root::codec::Encode>() {}
                #call;
            };
        });
    }
    quote! { #(#asserts)* }
}

fn is_unit(ty: &Type) -> bool {
    matches!(ty, Type::Tuple(t) if t.elems.is_empty())
}

fn is_stream_method(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| is_istmo_leaf_attr(a, "stream"))
}

fn is_owned_method(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|a| is_istmo_leaf_attr(a, "owned"))
}

fn is_istmo_leaf_attr(attr: &Attribute, tail: &str) -> bool {
    let path = attr.path();
    if path.is_ident(tail) {
        return true;
    }
    if path.segments.len() == 2 {
        let first = &path.segments[0].ident;
        let second = &path.segments[1].ident;
        return (first == "istmo" || first == "istmo_macros") && second == tail;
    }
    false
}

fn expand_unary_method(plugin_id: &LitStr, root: &Path, spec: &MethodSpec) -> TokenStream {
    let MethodSpec {
        name,
        method_name_str,
        doc_attrs,
        ..
    } = spec;
    let params = spec.client_params();
    let payload_expr = build_payload_expr(root, spec);
    let ok_ty = spec.ret.ok_ty();
    let decode = quote_spanned! {ok_ty.span()=> #root::codec::decode::<#ok_ty>(&bytes) };
    quote! {
        #(#doc_attrs)*
        pub async fn #name(&self, #(#params),*) -> ::core::result::Result<
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
                    let (value, _) = #decode?;
                    ::core::result::Result::Ok(value)
                }
                ::core::result::Result::Err(bytes) => {
                    ::core::result::Result::Err(#root::IstmoError::PluginError { bytes })
                }
            }
        }
    }
}

fn expand_unary_owned_method(root: &Path, spec: &MethodSpec) -> syn::Result<TokenStream> {
    let MethodSpec {
        name, doc_attrs, ..
    } = spec;
    let owned_name = format_ident!("{}_owned", name);
    let params = spec.client_params();
    let arg_names = spec.wire_arg_names();
    let (owned_return_ty, adopter_expr) = build_owned_return(spec.ret.ok_ty())?;
    Ok(quote! {
        #(#doc_attrs)*
        pub async fn #owned_name(
            &self,
            #(#params),*
        ) -> ::core::result::Result<#owned_return_ty, #root::IstmoError> {
            let __wire = self.#name(#(#arg_names),*).await?;
            let __rt = &self.__runtime;
            ::core::result::Result::Ok(#adopter_expr)
        }
    })
}

fn expand_stream_method(plugin_id: &LitStr, root: &Path, spec: &MethodSpec) -> TokenStream {
    let MethodSpec {
        name,
        method_name_str,
        doc_attrs,
        ..
    } = spec;
    let params = spec.client_params();
    let payload_expr = build_payload_expr(root, spec);
    let item_ty = spec.ret.ok_ty();
    quote! {
        #(#doc_attrs)*
        pub fn #name(&self, #(#params),*) -> ::core::result::Result<
            #root::TypedStream<#item_ty>,
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

fn build_owned_return(ok_ty: &Type) -> syn::Result<(TokenStream, TokenStream)> {
    if let Some(inner) = as_generic("Option", ok_ty)
        && let Some(owned) = owned_sibling(inner)
    {
        return Ok((
            quote! { ::core::option::Option<#owned> },
            quote! { __wire.map(|v| v.into_owned(__rt)) },
        ));
    }

    if let Some(inner) = as_generic("Vec", ok_ty)
        && let Some(owned) = owned_sibling(inner)
    {
        return Ok((
            quote! { ::std::vec::Vec<#owned> },
            quote! {
                __wire.into_iter().map(|v| v.into_owned(__rt)).collect::<::std::vec::Vec<_>>()
            },
        ));
    }

    if let Some(owned) = owned_sibling(ok_ty) {
        return Ok((quote! { #owned }, quote! { __wire.into_owned(__rt) }));
    }

    Err(syn::Error::new_spanned(
        ok_ty,
        "`#[istmo::owned]` supports return shapes `T`, `Option<T>` or `Vec<T>` where `T` \
         is a `#[message]` struct with `#[handle]` fields",
    ))
}

/// The `Owned*` sibling `#[message]` emits next to a wire struct:
/// `api::Account<u8>` → `api::OwnedAccount<u8>`.
fn owned_sibling(ty: &Type) -> Option<Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let last = path.segments.last()?;
    if last.ident == "Option" || last.ident == "Vec" {
        return None;
    }
    let mut owned = path.clone();
    let last = owned.segments.last_mut()?;
    last.ident = format_ident!("Owned{}", last.ident, span = last.ident.span());
    Some(Type::Path(TypePath {
        qself: None,
        path: owned,
    }))
}

fn as_generic<'a>(wrapper: &str, ty: &'a Type) -> Option<&'a Type> {
    let Type::Path(TypePath { qself: None, path }) = ty else {
        return None;
    };
    let seg = path.segments.last()?;
    if seg.ident != wrapper {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &seg.arguments else {
        return None;
    };
    args.args.iter().find_map(|a| match a {
        GenericArgument::Type(t) => Some(t),
        _ => None,
    })
}
