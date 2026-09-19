//! Android JNI transport for the [`istmo`](https://docs.rs/istmo)
//! runtime.
//!
//! Links against `android_activity` and exposes the JNI symbols the
//! companion Kotlin runtime library binds to. Consumer apps rarely
//! import this crate directly — the top-level facade re-exports it as
//! `istmo::android` on `target_os = "android"`.
//!
//! The crate is deliberately narrow: it owns the JNI pump thread, the
//! main-thread bridge, and the byte-oriented wire between the Rust
//! runtime and Kotlin. Everything else is regular istmo runtime code.

#![doc(html_root_url = "https://docs.rs/istmo-android")]

#[cfg(target_os = "android")]
pub mod app;
pub mod entrypoint;
pub mod error;

mod jni_exports;
mod pump;
mod state;

#[cfg(target_os = "android")]
pub use android_activity;
#[cfg(target_os = "android")]
pub use app::{android_activity_object, android_app, set_android_app};
pub use error::AndroidRuntimeError;

pub use jni_exports::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyQueue,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};

