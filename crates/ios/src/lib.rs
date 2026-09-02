//! `@_cdecl` transport for the istmo runtime on Apple platforms.
//!
//! Mirrors the shape of [`istmo_android`] but drops the JVM machinery in
//! favour of a plain C ABI. Swift consumes the exported symbols as
//! `@_silgen_name(...)` / `@_cdecl(...)` functions; the Rust pump drives the
//! Swift side back through function pointers registered at startup.
//!
//! ## FFI surface
//!
//! Every symbol below is `#[unsafe(no_mangle)] pub extern "C"` and named with
//! the `istmo_ios_` prefix so `@_silgen_name("istmo_ios_start")` etc. on the
//! Swift side lines up verbatim.
//!
//! Inbound (Swift → Rust):
//!
//! * `istmo_ios_start(callbacks: IstmoIosCallbacks) -> bool`
//! * `istmo_ios_shutdown()`
//! * `istmo_ios_submit_response(call_id, ok, payload, len)`
//! * `istmo_ios_submit_event(stream_id, payload, len)`
//! * `istmo_ios_submit_stream_end(stream_id, reason, err_payload, err_len)`
//! * `istmo_ios_submit_call(call_id, plugin_id_utf8, plugin_len, instance_id,
//!   method_utf8, method_len, payload, payload_len)`
//! * `istmo_ios_submit_early_latest(channel_utf8, channel_len, payload, len)`
//! * `istmo_ios_submit_early_queue(channel_utf8, channel_len, capacity,
//!   payload, len)`
//!
//! Outbound (Rust → Swift) travels through the [`IstmoIosCallbacks`] table.
//! The pump thread invokes those callbacks per drained [`istmo_core::Frame`];
//! Swift wraps them into `async` / `AsyncThrowingStream` primitives.
//!
//! ## Why a callback table instead of statically-named Swift symbols
//!
//! On Android the pump can look up static Kotlin methods by name on a
//! `GlobalRef<Class>`. Apple's linker has no equivalent runtime-lookup path
//! that also works in every Swift target (`@_cdecl` symbols are visible only
//! at final-link time and require the crate to link against a static Swift
//! archive, which we cannot assume during unit testing). A `#[repr(C)]`
//! table of function pointers registered at `start()` sidesteps the
//! platform-linker ordering entirely and keeps the crate exercisable from
//! pure-Rust integration tests (see `crates/ios/tests/`).

pub mod entrypoint;
pub mod error;

mod cdecl_exports;
mod pump;
mod state;

pub use cdecl_exports::{
    IstmoIosCallbacks, istmo_ios_shutdown, istmo_ios_start, istmo_ios_submit_call,
    istmo_ios_submit_early_latest, istmo_ios_submit_early_queue, istmo_ios_submit_event,
    istmo_ios_submit_response, istmo_ios_submit_stream_end,
};
pub use error::IosRuntimeError;
