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

pub mod error;

mod jni_exports;
mod pump;
mod state;

pub use error::AndroidRuntimeError;

// Re-export the JNI trampolines so the downstream cdylib retains them even
// under aggressive dead-code stripping. Users don't call these directly —
// they exist so the `.so` exposes the symbols the JVM looks up.
pub use jni_exports::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};
