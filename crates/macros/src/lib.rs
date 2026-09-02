//! Proc-macros for istmo.
//!
//! Two entry points are exposed:
//!
//! * [`plugin`] — attribute macro applied to a trait; replaces the trait
//!   with a client struct wired to the process runtime.
//! * [`message`] — attribute macro applied to a type; prepends the
//!   `bincode::Encode` / `bincode::Decode` derives so that the type can
//!   cross the wire without the author having to write two derives every
//!   time. See `CLAUDE.md` for the trade-off with a native
//!   `#[derive(istmo::Message)]` derive (deferred until we need the extra
//!   flexibility).
//!
//! The [`stream`] attribute is a compile-time marker consumed by
//! [`plugin`]. Defining it as a real proc-macro attribute keeps
//! `rust-analyzer` happy and prevents unknown-attribute errors when the
//! trait is inspected out of the plugin macro's context.

use proc_macro::TokenStream;

mod message;
mod plugin;
mod runtime;
mod service;
mod worker;

/// Turn a trait declaration into an istmo plugin client. See `PLAN.md`
/// for the design.
#[proc_macro_attribute]
pub fn plugin(attr: TokenStream, item: TokenStream) -> TokenStream {
    match plugin::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Prepend `#[derive(::istmo::bincode::Encode, ::istmo::bincode::Decode)]`
/// to a type declaration so the type participates in the wire codec.
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    match message::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Marker attribute placed on a trait method to declare that it is a
/// stream, not a unary call. Consumed by [`plugin`]; expands to a no-op
/// so the annotated trait method still parses on its own.
#[proc_macro_attribute]
pub fn stream(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Turn a trait declaration (`on_start` + optional `on_stop`) into a service
/// adapter. See [`service`] source for the design.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    match service::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Turn a trait declaration with a single `async fn run(&self, ctx:
/// WorkerContext) -> Result<TaskOutcome, _>` method into a WorkManager-shaped
/// worker adapter. See [`worker`] source for the design.
#[proc_macro_attribute]
pub fn worker(attr: TokenStream, item: TokenStream) -> TokenStream {
    match worker::expand(attr.into(), item.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}

/// Single wiring point for a cdylib / binary.
///
/// Declares the plugin ids the process consumes (`plugins:`) and the trait
/// implementations it hosts (`hosts:`), and emits the
/// `__istmo_configure_runtime` function that `nativeStart` invokes on
/// android. See [`runtime`] source for the full expansion.
#[proc_macro]
pub fn runtime(input: TokenStream) -> TokenStream {
    match runtime::expand(input.into()) {
        Ok(tokens) => tokens.into(),
        Err(err) => err.into_compile_error().into(),
    }
}
