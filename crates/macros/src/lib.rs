//! Proc-macros powering [`istmo`](https://docs.rs/istmo).
//!
//! Most users pull these in through the top-level facade — the
//! attributes are re-exported as `istmo::plugin`, `istmo::message`,
//! `istmo::stream`, `istmo::owned`, `istmo::handle`, `istmo::redact`,
//! `istmo::service`,
//! `istmo::worker`, `istmo::mobile_app`, and the function-like macros
//! `istmo::runtime!` and `istmo::app_icon!`.
//!
//! # What each macro emits
//!
//! - [`#[plugin]`](plugin) — turns an async trait into three siblings:
//!   the trait itself (with `async fn` desugared to
//!   `impl Future + Send + '_`), a `<Trait>Client` type that calls into the
//!   [`Runtime`](https://docs.rs/istmo-core/latest/istmo_core/struct.Runtime.html)
//!   for the caller side, and a `<Trait>Host<Impl>` dispatcher that
//!   wires a Rust implementation to inbound frames.
//! - [`#[message]`](message) — a thin sugar over
//!   `#[derive(wincode::SchemaWrite, wincode::SchemaRead)]` for wire-safe
//!   payloads.
//! - [`#[stream]`](stream) — marks a plugin method as returning a live
//!   stream. The base trait return type is the stream item.
//! - [`#[handle(T)]`](handle) — on a wire-struct field, requests that
//!   the accompanying `_owned` variant materialize the field into a
//!   [`NativeHandle<T>`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html).
//! - [`#[redact]`](redact) — on a wire-struct field, hides it from the
//!   `Debug` impl `#[message]` writes for the struct.
//! - [`#[owned]`](owned) — on a trait method, opts into the `_owned`
//!   client sibling that adopts every returned handle id into a
//!   `NativeHandle`.
//! - [`#[service]`](service) — declarative long-running background
//!   service with `on_start` / `on_stop` hooks.
//! - [`#[worker]`](worker) — declarative one-shot scheduled task.
//! - [`#[mobile_app]`](mobile_app) — entry-point glue for the mobile
//!   transports: emits the JNI / FFI symbols the platform side loads on
//!   start.
//! - [`runtime!`](runtime) — declarative wiring of the process-global
//!   runtime, including auto-wired plugins picked up from
//!   `istmo.toml` manifests.

#![expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]

use proc_macro::TokenStream;

mod common;
mod message;
mod mobile_app;
mod plugin;
mod runtime;
mod service;
mod worker;

/// Turn an async trait declaration into an istmo plugin.
///
/// Emits three sibling items in the surrounding module:
///
/// - `<Trait>` — the original trait with every `async fn` desugared into
///   a `fn` returning `impl Future<Output = T> + Send + '_` (the future
///   borrows `&self`). Implementations still write `async fn`.
/// - `<Trait>Client` — the caller-side type that encodes arguments,
///   invokes the runtime, and decodes responses. Constructed with
///   `<Trait>Client::from_runtime(&rt)` (stateless) or
///   `<Trait>Client::from_runtime_with(&rt, config).await` (stateful).
/// - `<Trait>Host<Impl: Trait>` — the dispatcher that plugs a Rust
///   implementation of the trait into the runtime. Stateful plugins get
///   a `<Trait>Factory` trait and a `<Trait>Host<F: <Trait>Factory>`
///   instead.
///
/// # Attributes
///
/// - `name = "plugin.id"` — stable wire identifier (required).
/// - `init = SomeConfig` — declare a stateful plugin whose instances
///   are constructed via
///   [`Runtime::create_instance`](https://docs.rs/istmo-core/latest/istmo_core/struct.Runtime.html#method.create_instance).
/// - `crate = "::istmo_core"` — path the generated code reaches the
///   runtime through. Defaults to `::istmo`; crates that depend on
///   `istmo-core` directly instead of the facade override it.
///
/// # Methods
///
/// - Unary methods are `async fn(&self, ...) -> T`. Each argument and
///   the return type must be a wire type (`#[message]` or a primitive).
/// - Stream methods are plain `fn` marked [`#[stream]`](macro@stream);
///   see that attribute.
/// - The receiver must be exactly `&self`. Generic methods, default
///   method bodies, associated types / consts, generic traits and
///   supertraits are rejected.
///
/// # Return types and domain errors
///
/// A return type written `Result<T, E>` (any path ending in `Result`
/// with exactly two type arguments) is split: `T` travels as the success
/// payload and `E` as a domain error, surfaced on the client as
/// `IstmoError::PluginError { bytes }`. `E` must implement
/// [`std::error::Error`]; the macro asserts it. `Result<T, ()>` is exempt.
///
/// A `Result` with fewer arguments — `io::Result<T>`, or a local
/// `type Result<T> = …` — is a compile error: the hidden error type
/// would otherwise be encoded differently by client and host. An alias
/// under another name (`type Reply<T> = Result<T, MyError>`) is opaque
/// to the macro and travels whole as the success payload.
///
/// # Cooperative cancellation
///
/// An argument whose type's last path segment is `CancelToken` is
/// stripped from the wire and from the client signature; the host fills
/// it with the runtime's per-call token. Detection is by name, so:
///
/// - an alias under another name (`type Stop = CancelToken;`),
///   `&CancelToken` or `Option<CancelToken>` is *not* detected and is
///   treated as a wire argument, which fails to compile because the token
///   is not a wire type;
/// - an unrelated type that happens to be named `CancelToken` *is*
///   detected and fails to compile with a type mismatch against
///   `istmo::CancelToken`.
///
/// # Example
///
/// ```
/// use istmo::{IstmoError, message, plugin};
///
/// #[message]
/// #[derive(Debug)]
/// pub struct EchoError {
///     pub reason: String,
/// }
///
/// impl std::fmt::Display for EchoError {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         f.write_str(&self.reason)
///     }
/// }
///
/// impl std::error::Error for EchoError {}
///
/// #[message]
/// pub struct Ping {
///     pub payload: String,
/// }
///
/// #[plugin(name = "demo.echo")]
/// pub trait Echo {
///     async fn echo(&self, ping: Ping) -> Result<String, EchoError>;
/// }
///
/// struct Loopback;
///
/// impl Echo for Loopback {
///     async fn echo(&self, ping: Ping) -> Result<String, EchoError> {
///         Ok(ping.payload)
///     }
/// }
///
/// let rt = istmo::Runtime::mock().runtime;
/// rt.register_host(EchoHost::new(Loopback));
/// let client = EchoClient::from_runtime(&rt)?;
/// # Ok::<(), IstmoError>(())
/// ```
#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    match plugin::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Mark a struct or enum as a wire-safe message.
///
/// Sugar over `#[derive(wincode::SchemaWrite, wincode::SchemaRead)]` — the same
/// derives are emitted, so message types can be constructed and
/// inspected as ordinary Rust values but also travel across the wire.
///
/// # Attributes
///
/// - `crate = "::istmo_core"` — runtime path for the `Owned*` sibling.
///   Defaults to `::istmo`.
/// - `wincode = "::wincode"` — wincode path for the derives. Defaults to
///   `<crate>::wincode`.
///
/// A named-field struct whose fields carry [`#[handle(T)]`](macro@handle)
/// also gets an `Owned<Name>` sibling and an `into_owned(&rt)` method.
/// Fields marked [`#[redact]`](macro@redact) print as `<redacted>` in the
/// `Debug` impl the macro then writes for the struct and its sibling.
///
/// # Example
///
/// ```
/// use istmo::message;
///
/// #[message]
/// pub struct Coords {
///     pub x: f32,
///     pub y: f32,
/// }
///
/// let bytes = istmo::codec::encode(&Coords { x: 1.0, y: 2.0 })?;
/// let (back, _) = istmo::codec::decode::<Coords>(&bytes)?;
/// assert_eq!(back.y, 2.0);
/// # Ok::<(), istmo::CodecError>(())
/// ```
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    match message::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Marker for stream-returning plugin methods.
///
/// Attach to a non-`async` trait method whose declared return type is
/// the stream item. The macro rewrites the trait's return type to a
/// [`flume::Receiver`](https://docs.rs/flume/latest/flume/struct.Receiver.html)
/// of that item: the implementation hands back the receiver, produces
/// items on any thread and drops the sender to complete the stream. The
/// client method returns a `TypedStream<Item>`.
///
/// An item written `Result<T, E>` makes the stream fallible: `Ok(t)`
/// items travel as events, and the first `Err(e)` ends the stream with
/// `StreamEnd::Error` carrying `e` (nothing sent after it is read). The
/// client method then returns a `TypedStream<T, E>`, whose
/// `StreamItem::Failed(e)` is that error decoded. As for unary methods,
/// `E` must implement [`std::error::Error`] unless it is `()`.
///
/// ```
/// #[istmo::message]
/// #[derive(Debug)]
/// pub struct Lost;
///
/// impl std::fmt::Display for Lost {
///     fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
///         f.write_str("signal lost")
///     }
/// }
///
/// impl std::error::Error for Lost {}
///
/// #[istmo::plugin(name = "demo.gps")]
/// pub trait Gps {
///     #[istmo::stream]
///     fn fixes(&self) -> Result<u32, Lost>;
/// }
///
/// struct Receiver;
///
/// impl Gps for Receiver {
///     fn fixes(&self) -> flume::Receiver<Result<u32, Lost>> {
///         let (tx, rx) = flume::unbounded();
///         let _ = tx.send(Ok(1));
///         let _ = tx.send(Err(Lost));
///         rx
///     }
/// }
/// ```
///
/// # Example
///
/// ```
/// #[istmo::plugin(name = "demo.ticker")]
/// pub trait Ticker {
///     #[istmo::stream]
///     fn ticks(&self, count: u32) -> u32;
/// }
///
/// struct Counter;
///
/// impl Ticker for Counter {
///     fn ticks(&self, count: u32) -> flume::Receiver<u32> {
///         let (tx, rx) = flume::unbounded();
///         for tick in 0..count {
///             let _ = tx.send(tick);
///         }
///         rx
///     }
/// }
/// ```
#[proc_macro_attribute]
pub fn stream(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marker for [`NativeHandle`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html)-carrying fields.
///
/// Attaches a phantom type to a
/// [`NativeHandleId`](https://docs.rs/istmo-core/latest/istmo_core/protocol/struct.NativeHandleId.html)
/// field of a named-field `#[message]` struct so the sibling `Owned*`
/// type can materialize it into a typed `NativeHandle<T>` on the Rust
/// side. Rejected on tuple structs and inside enums.
#[proc_macro_attribute]
pub fn handle(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marker for secret fields of a named-field `#[message]` struct.
///
/// `#[message]` then writes the struct's `Debug` impl itself (so the
/// struct must not derive `Debug`), printing `<redacted>` in place of
/// every marked field; the `Owned*` sibling, if any, follows suit.
/// Rejected on tuple structs and inside enums.
///
/// ```
/// #[istmo::message]
/// pub struct Login {
///     pub user: String,
///     #[redact]
///     pub password: String,
/// }
///
/// let login = Login { user: "ana".into(), password: "hunter2".into() };
/// assert_eq!(
///     format!("{login:?}"),
///     r#"Login { user: "ana", password: <redacted> }"#,
/// );
/// ```
#[proc_macro_attribute]
pub fn redact(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Emit an `_owned` client method that adopts returned native handles
/// into typed [`NativeHandle`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html)s.
///
/// Supported return shapes are `T`, `Option<T>` and `Vec<T>` (or a
/// `Result` of one of them), where `T` is a `#[message]` struct with
/// [`#[handle]`](macro@handle) fields. Not supported on stream methods.
///
/// # Example
///
/// ```
/// use istmo::{NativeHandleId, message, plugin};
///
/// pub struct Credential(std::convert::Infallible);
///
/// #[message]
/// pub struct Account {
///     pub email: String,
///     #[handle(Credential)]
///     pub credential: NativeHandleId,
/// }
///
/// #[plugin(name = "demo.sign_in")]
/// pub trait SignIn {
///     #[istmo::owned]
///     async fn sign_in(&self) -> Account;
/// }
///
/// // `credential` comes back as a `NativeHandle<Credential>` that
/// // releases the platform object when dropped.
/// async fn sign_in(client: &SignInClient) -> Result<OwnedAccount, istmo::IstmoError> {
///     client.sign_in_owned().await
/// }
/// ```
#[proc_macro_attribute]
pub fn owned(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Emit a long-running background service adapter.
///
/// The annotated trait declares `async fn on_start(&self, ctx:
/// ServiceContext)` (optionally returning `Result<(), E>`) and an
/// optional `async fn on_stop(&self)`. The macro generates a
/// `<Trait>Adapter<Impl>` implementing `Dispatch`: `on_start` runs on a
/// dedicated thread and is stopped (and joined) before a second
/// `on_start` or on `on_stop`, and a `Frame::Cancel` stops it through
/// the context.
///
/// # Attributes
///
/// - `name = "service.id"` — stable wire identifier (required).
/// - `crate = "::istmo"` — runtime path. Defaults to `::istmo`.
/// - `plugins = "::istmo::plugins"` — path to the first-party plugin
///   surfaces (`ServiceContext`, `StopNotifier`). Defaults to
///   `<crate>::plugins`; crates that set `crate = "::istmo_core"` also
///   set `plugins = "::istmo_plugins"`.
///
/// # Example
///
/// ```
/// use istmo::plugins::ServiceContext;
///
/// #[istmo::service(name = "demo.sync")]
/// pub trait SyncService {
///     async fn on_start(&self, ctx: ServiceContext);
/// }
///
/// struct Syncer;
///
/// impl SyncService for Syncer {
///     async fn on_start(&self, ctx: ServiceContext) {
///         ctx.stopped().await;
///     }
/// }
///
/// let rt = istmo::Runtime::mock().host(SyncServiceAdapter::new(Syncer)).finish();
/// ```
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    match service::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Emit a one-shot scheduled worker adapter.
///
/// The annotated trait declares `async fn run(&self, ctx: WorkerContext)
/// -> TaskOutcome` (or `Result<TaskOutcome, E>`); the macro generates a
/// `<Trait>WorkerAdapter<Impl>` that decodes the scheduled task's input,
/// awaits `run`, and encodes the outcome as a unary response. Takes the
/// same `name` / `crate` / `plugins` attributes as
/// [`#[service]`](macro@service).
///
/// # Example
///
/// ```
/// use istmo::plugins::{TaskOutcome, WorkerContext};
///
/// #[istmo::worker(name = "demo.backup")]
/// pub trait Backup {
///     async fn run(&self, ctx: WorkerContext) -> TaskOutcome;
/// }
///
/// struct Nightly;
///
/// impl Backup for Nightly {
///     async fn run(&self, _ctx: WorkerContext) -> TaskOutcome {
///         TaskOutcome::Success
///     }
/// }
///
/// let rt = istmo::Runtime::mock().host(BackupWorkerAdapter::new(Nightly)).finish();
/// ```
#[proc_macro_attribute]
pub fn worker(attr: TokenStream, item: TokenStream) -> TokenStream {
    match worker::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Emit the platform entry-point symbols for a mobile app.
///
/// Placed on a zero-argument `fn`. On Android it emits `android_main`,
/// which stores the `AndroidApp` and calls the function; on Apple
/// targets it emits `extern "C" fn istmo_run_ios()`. Other targets get
/// the function unchanged.
#[proc_macro_attribute]
pub fn mobile_app(attr: TokenStream, item: TokenStream) -> TokenStream {
    match mobile_app::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Declarative wiring of the process-global runtime.
///
/// Invoked once at module level in the app crate. Emits the platform
/// entry-point re-exports and `pub fn __istmo_configure_runtime(init:
/// RuntimeInit) -> RuntimeInit`, which the transport (or a test) calls to
/// apply the declarations below. Every section is optional:
///
/// - `plugins: [EchoClient, api::LifecycleClient, ...]` — clients this
///   runtime intends to call (paths to `<Trait>Client` types).
/// - `remote: [Compute, api::StoreClient, ...]` — plugins whose frames
///   are routed through the registered
///   [remote envelope sink](https://docs.rs/istmo-core/latest/istmo_core/type.RemoteEnvelopeSink.html).
///   Each entry is either the plugin trait (`Compute`, which becomes
///   `ComputeClient`) or the client itself (a path whose last segment
///   ends in `Client`, used as-is). A trait whose own name ends in
///   `Client` must therefore be written as its full client name.
/// - `hosts: [Echo => EchoImpl, api::Store => Store::default()]` —
///   Rust implementations registered as `<Trait>Host::new(expr)`.
/// - `services: [SyncService => Syncer]` / `workers: [Backup => Nightly]` —
///   registered as `<Trait>Adapter::new(expr)` /
///   `<Trait>WorkerAdapter::new(expr)` for `#[service]` / `#[worker]`
///   traits.
///
/// Auto-wiring: client paths that `istmo_build::emit_wiring_env` puts in
/// `ISTMO_AUTO_PLUGINS` / `ISTMO_AUTO_REMOTE` are added to the `plugins:`
/// / `remote:` sets. Duplicate declarations are deduplicated.
///
/// # Example
///
/// ```
/// #[istmo::plugin(name = "demo.echo")]
/// pub trait Echo {
///     async fn echo(&self, text: String) -> String;
/// }
///
/// #[istmo::plugin(name = "demo.compute")]
/// pub trait Compute {
///     async fn crunch(&self, input: u32) -> u32;
/// }
///
/// struct Loopback;
///
/// impl Echo for Loopback {
///     async fn echo(&self, text: String) -> String {
///         text
///     }
/// }
///
/// istmo::runtime!(
///     plugins: [EchoClient],
///     remote: [Compute],
///     hosts: [Echo => Loopback],
/// );
///
/// let init = __istmo_configure_runtime(istmo::Runtime::mock());
/// assert!(init.runtime.is_remote_plugin("demo.compute"));
/// EchoClient::from_runtime(&init.runtime)?;
/// # Ok::<(), istmo::IstmoError>(())
/// ```
#[proc_macro]
pub fn runtime(input: TokenStream) -> TokenStream {
    match runtime::expand(input.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// The app's desktop icon as PNG bytes, for the window icon.
///
/// `istmo_build::emit()` stages a 256 px PNG from `[app.icon]`
/// (`desktop`, else `image`) when building for a desktop target and
/// points `ISTMO_APP_ICON_PNG` at it. Expands to an
/// `Option<&'static [u8]>`: `Some` with the embedded bytes, `None` on
/// Android / iOS or when the app sets no icon.
///
/// # Example
///
/// ```
/// let icon: Option<&'static [u8]> = istmo::app_icon!();
/// if let Some(png) = icon {
///     assert!(!png.is_empty());
/// }
/// ```
#[proc_macro]
pub fn app_icon(input: TokenStream) -> TokenStream {
    if let Some(token) = proc_macro2::TokenStream::from(input).into_iter().next() {
        return syn::Error::new(token.span(), "`app_icon!()` takes no arguments")
            .into_compile_error()
            .into();
    }
    // `option_env!` records the variable in the caller's dep-info, so a
    // new or moved icon rebuilds the crate.
    let tracked = quote::quote! {
        const _: ::core::option::Option<&str> = ::core::option_env!("ISTMO_APP_ICON_PNG");
    };
    match std::env::var("ISTMO_APP_ICON_PNG") {
        Ok(path) if !path.is_empty() => quote::quote! {
            {
                #tracked
                ::core::option::Option::Some(::core::include_bytes!(#path) as &'static [u8])
            }
        },
        _ => quote::quote! {
            {
                #tracked
                ::core::option::Option::<&'static [u8]>::None
            }
        },
    }
    .into()
}
