//! Android JNI transport for the [`istmo`](https://docs.rs/istmo)
//! runtime.
//!
//! Links against `android_activity` and exposes the JNI symbols the
//! companion Kotlin runtime library binds to. Consumer apps rarely
//! import this crate directly — the top-level facade re-exports it as
//! `istmo::android` on `target_os = "android"`.
//!
//! The crate is deliberately narrow: it owns the JNI pump thread, the
//! byte-oriented wire between the Rust runtime and Kotlin, and the
//! JNI hooks that install the app's assets and directories. Everything else is regular istmo runtime code.

#![doc(html_root_url = "https://docs.rs/istmo-android")]

/// Stamp a `latency_probe::Stage`; expands to nothing unless the
/// `latency-probe` feature is on.
macro_rules! probe {
    ($stage:ident) => {
        #[cfg(feature = "latency-probe")]
        $crate::latency_probe::stamp($crate::latency_probe::Stage::$stage);
    };
}

#[cfg(feature = "latency-probe")]
pub mod latency_probe;

#[cfg(target_os = "android")]
pub mod app;
pub mod assets;
pub mod entrypoint;
pub mod error;
pub mod path;

mod frame_exports;
mod frame_producer;
mod jni_exports;
mod native_buffer;
mod pump;
mod state;

#[cfg(target_os = "android")]
pub use android_activity;
#[cfg(target_os = "android")]
pub use app::{android_activity_object, android_app, set_android_app};
pub use error::AndroidRuntimeError;
pub use frame_exports::{
    Java_dev_istmo_runtime_IstmoFrames_nativeAbiVersion,
    Java_dev_istmo_runtime_IstmoFrames_nativeAcquireSlot,
    Java_dev_istmo_runtime_IstmoFrames_nativeAwaitReleased,
    Java_dev_istmo_runtime_IstmoFrames_nativeEndProducer,
    Java_dev_istmo_runtime_IstmoFrames_nativeFreeProducer,
    Java_dev_istmo_runtime_IstmoFrames_nativeOpenProducer,
    Java_dev_istmo_runtime_IstmoFrames_nativeOpenSlotPool,
    Java_dev_istmo_runtime_IstmoFrames_nativeReleaseSlot,
    Java_dev_istmo_runtime_IstmoFrames_nativeSubmitBytes,
    Java_dev_istmo_runtime_IstmoFrames_nativeSubmitDirectBuffer,
    Java_dev_istmo_runtime_IstmoFrames_nativeSubmitHardwareBuffer,
    Java_dev_istmo_runtime_IstmoFrames_nativeSubmitSlot,
};
pub use frame_producer::PoolSlot;
pub use native_buffer::{HardwareBuffer, HardwareBufferDesc};

pub use jni_exports::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeInjectEnvelope,
    Java_dev_istmo_runtime_IstmoRuntime_nativeInstallAssets,
    Java_dev_istmo_runtime_IstmoRuntime_nativeInstallPaths,
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCallDirect,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCancel,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyQueue,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitNotify,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};
