//! JNI transport for the istmo runtime.
//!
//! The Kotlin side (a singleton typically named `dev.istmo.runtime.IstmoRuntime`)
//! exposes three native methods:
//!
//! * `nativeStart(runtimeClass: Class<*>): Boolean` — initialises the process
//!   runtime, caches the `JavaVM` and the `onOutboundFrame(ByteArray)` method
//!   id on the Kotlin class, spawns the outbound pump thread.
//! * `nativeSubmitFrame(frame: ByteArray)` — decodes and dispatches an inbound
//!   envelope (typically `Respond`, `Event` or `StreamEnd`).
//! * `nativeShutdown()` — cancels every pending call / stream and stops the
//!   pump thread.
//!
//! The pump thread drains [`istmo_core::Envelope`] values off the runtime's
//! outbound channel and delivers them to Kotlin via
//! `IstmoRuntime.onOutboundFrame(ByteArray)`.
//!
//! `nativeStart` currently wires an [`istmo_core::InlineMainThread`]
//! dispatcher: nothing in the framework pushes work back onto the Android
//! main thread yet. A `Handler`-backed [`istmo_core::MainThread`] will land
//! alongside the first plugin that needs it (M3 lifecycle / permissions).

pub mod entrypoint;
pub mod error;

mod jni_exports;
mod pump;
mod state;

pub use error::AndroidRuntimeError;

// Trampolines are declared with `#[unsafe(no_mangle)]` in [`jni_exports`]
// which keeps them at the crate root. Downstream cdylibs consume them via
// [`entrypoint`], which the `istmo::runtime!` macro re-exports verbatim so
// user code never touches JNI-facing symbols directly.
pub use jni_exports::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};
