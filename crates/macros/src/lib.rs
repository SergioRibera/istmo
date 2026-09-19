//! Proc-macros powering [`istmo`](https://docs.rs/istmo).
//!
//! Most users pull these in through the top-level facade — the
//! attributes are re-exported as `istmo::plugin`, `istmo::message`,
//! `istmo::service`, `istmo::worker`, `istmo::mobile_app`, and the
//! function-like macro `istmo::runtime!`.
//!
//! # What each macro emits
//!
//! - [`#[plugin]`](plugin) — turns an async trait into three siblings:
//!   the trait itself (with `async fn` desugared to
//!   `impl Future + Send`), a `<Trait>Client` type that calls into the
//!   [`Runtime`](https://docs.rs/istmo-core/latest/istmo_core/struct.Runtime.html)
//!   for the caller side, and a `<Trait>Host<Impl>` dispatcher that
//!   wires a Rust implementation to inbound frames.
//! - [`#[message]`](message) — a thin sugar over
//!   `#[derive(bincode::Encode, bincode::Decode)]` for wire-safe
//!   payloads.
//! - [`#[stream]`](stream) — marks a plugin method as returning a live
//!   stream. The base trait return type is the stream item.
//! - [`#[handle(T)]`](handle) — on a wire-struct field, requests that
//!   the accompanying `_owned` variant materialize the field into a
//!   [`NativeHandle<T>`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html).
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

use proc_macro::TokenStream;

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
/// - `<Trait>` — the original trait with `async fn` desugared into a
///   `Send + 'static` future.
/// - `<Trait>Client` — the caller-side type that encodes arguments,
///   invokes the runtime, and decodes responses. Constructed with
///   `<Trait>Client::from_runtime(&rt)`.
/// - `<Trait>Host<Impl: Trait>` — the dispatcher that plugs a Rust
///   implementation of the trait into the runtime.
///
/// # Attributes
///
/// - `id = "plugin.id"` — stable wire identifier (required).
/// - `init = SomeConfig` — declare a stateful plugin whose instances
///   are constructed via
///   [`Runtime::create_instance`](https://docs.rs/istmo-core/latest/istmo_core/struct.Runtime.html#method.create_instance).
///
/// # Example
///
/// ```ignore
/// use istmo::{plugin, message, IstmoError};
///
/// #[message]
/// pub struct Ping { pub payload: String }
///
/// #[plugin(id = "demo.echo")]
/// pub trait Echo {
///     async fn echo(&self, ping: Ping) -> Result<String, IstmoError>;
/// }
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
/// Sugar over `#[derive(bincode::Encode, bincode::Decode)]` — the same
/// derives are emitted, so message types can be constructed and
/// inspected as ordinary Rust values but also travel across the wire.
///
/// # Example
///
/// ```ignore
/// use istmo::message;
///
/// #[message]
/// pub struct Coords { pub x: f32, pub y: f32 }
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
/// Attach to a trait method whose declared return type is the stream
/// item. The macro rewrites the return type to a
/// [`flume::Receiver`](https://docs.rs/flume/latest/flume/struct.Receiver.html)
/// on the host side and threads events through the runtime.
#[proc_macro_attribute]
pub fn stream(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Marker for [`NativeHandle`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html)-carrying fields.
///
/// Attaches a phantom type to a
/// [`NativeHandleId`](https://docs.rs/istmo-core/latest/istmo_core/protocol/struct.NativeHandleId.html)
/// field of a wire struct so the sibling `_owned` type can materialize
/// it into a typed `NativeHandle<T>` on the Rust side.
#[proc_macro_attribute]
pub fn handle(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Emit an `_owned` client method that adopts returned native handles
/// into typed [`NativeHandle`](https://docs.rs/istmo-core/latest/istmo_core/native_handle/struct.NativeHandle.html)s.
///
/// Supported return shapes are `T`, `Option<T>` and `Vec<T>`.
#[proc_macro_attribute]
pub fn owned(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Emit a long-running background service adapter.
///
/// The annotated impl block hosts `on_start` and `on_stop` hooks; the
/// macro generates a `Dispatch` implementation that owns the OS thread
/// running `on_start` and cooperatively cancels it when the platform
/// side asks the service to stop.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    match service::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Emit a one-shot scheduled worker adapter.
///
/// The annotated impl exposes a `run` hook; the macro decodes the
/// scheduled task's input, awaits `run`, and encodes the outcome as a
/// unary response.
#[proc_macro_attribute]
pub fn worker(attr: TokenStream, item: TokenStream) -> TokenStream {
    match worker::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Emit the platform entry-point symbols for a mobile app.
///
/// Called on the top-level `fn main` (desktop) or the app struct
/// (mobile). Wires Android's `AndroidApp` and iOS's `UIApplication`
/// bootstraps into a shared [`runtime!`] invocation.
#[proc_macro_attribute]
pub fn mobile_app(attr: TokenStream, item: TokenStream) -> TokenStream {
    match mobile_app::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Declarative wiring of the process-global runtime.
///
/// Accepts named sections:
///
/// - `plugins: [ClientA, ClientB, ...]` — plugins this runtime intends
///   to call.
/// - `hosts: [HostA::new(), HostB::default(), ...]` — Rust-hosted
///   dispatchers registered on the runtime.
/// - `services: [ServiceA, ...]` / `workers: [WorkerA, ...]` — plugin
///   surfaces backed by `#[service]` / `#[worker]` adapters.
/// - `remote: [ClientC, ...]` — plugins whose frames should be routed
///   through the registered
///   [remote envelope sink](https://docs.rs/istmo-core/latest/istmo_core/type.RemoteEnvelopeSink.html).
///
/// Auto-wiring: entries picked up from the crate's `istmo.toml` and
/// any plugin manifest reached through the `DEP_*_ISTMO_MANIFEST`
/// build-script handover are added to the `plugins:` / `remote:` sets
/// automatically. Duplicate declarations are deduplicated.
///
/// # Example
///
/// ```ignore
/// use istmo::runtime;
///
/// let rt = runtime! {
///     plugins: [EchoClient, LifecycleClient],
///     hosts: [MyEchoBackend::new()],
/// };
/// ```
#[proc_macro]
pub fn runtime(input: TokenStream) -> TokenStream {
    match runtime::expand(input.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}
