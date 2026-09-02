//! Entry-point re-exports consumed by the `istmo::runtime!` macro.
//!
//! The five JNI trampolines the JVM looks up on the demo `IstmoRuntime` class
//! are declared with `#[unsafe(no_mangle)]` in [`crate::jni_exports`]. A
//! downstream cdylib must expose them via `pub use` so `--gc-sections` on a
//! release build does not strip them out of the final `.so`. Rather than have
//! every user hand-write that block, [`istmo::runtime!`] emits
//! `pub use ::istmo_android::entrypoint::*;` — this module is the single
//! adjective-free surface the macro pins.
//!
//! Users never write `no_mangle`, `JNIEnv` or any `Java_…_native*` symbol.

pub use crate::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};
