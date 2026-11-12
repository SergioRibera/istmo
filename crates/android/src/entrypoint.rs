//! Every JNI symbol the Kotlin runtime binds to. `istmo::runtime!`
//! glob-re-exports this module from the app crate so the linker keeps
//! all of them in the final cdylib; keep it in sync with the crate root.

pub use crate::{
    Java_dev_istmo_runtime_IstmoRuntime_nativeInjectEnvelope,
    Java_dev_istmo_runtime_IstmoRuntime_nativeInstallAssets,
    Java_dev_istmo_runtime_IstmoRuntime_nativeInstallPaths,
    Java_dev_istmo_runtime_IstmoRuntime_nativeShutdown,
    Java_dev_istmo_runtime_IstmoRuntime_nativeStart,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCall,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitCancel,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyLatest,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEarlyQueue,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitEvent,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitNotify,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitResponse,
    Java_dev_istmo_runtime_IstmoRuntime_nativeSubmitStreamEnd,
};
