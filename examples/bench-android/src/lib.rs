//! On-device istmo latency benchmark.
//!
//! The Kotlin activity starts the real istmo runtime (JNI pump and all),
//! registers the generated `KotlinBench` dispatcher and drives the
//! matrix; this crate hosts the Rust ends:
//!
//! - [`hosts`]: echo dispatchers in each `DispatchMode`, a typed
//!   shortcut and the generated `RustBench` plugin Kotlin calls;
//! - [`cells`]: cells Rust drives (Rust → Kotlin calls, streams and
//!   frame sinks, legacy in-process modes);
//! - [`wake`]: thread hand-off baselines with no istmo involved;
//! - `jni` (Android): the `BenchNative` entry points.

#![allow(
    non_snake_case,
    reason = "JNI entry points follow the Java naming scheme"
)]

pub mod cells;
pub mod contracts;
pub mod hosts;
pub mod sys;
pub mod wake;

#[cfg(target_os = "android")]
mod jni;

use std::sync::Arc;

use istmo::{DispatchMode, Runtime};

use crate::contracts::{KotlinBenchClient, RustBenchHost};
use crate::hosts::{
    EchoDispatch, EchoTyped, ID_INLINE, ID_ISOLATED, ID_TYPED, RustBenchImpl, TypedEcho,
};

// Keep every transport symbol the Kotlin runtime binds in this cdylib.
#[cfg(target_os = "android")]
#[allow(
    unused_imports,
    reason = "the JVM resolves these symbols, not Rust code"
)]
pub use istmo::android::entrypoint::*;

/// What `istmo::runtime!` would emit; written by hand because the bench
/// also registers dispatchers that are not generated plugin hosts.
#[unsafe(no_mangle)]
#[expect(
    clippy::no_mangle_with_rust_abi,
    reason = "istmo-android declares the hook `extern \"Rust\"`"
)]
pub fn __istmo_configure_runtime(init: istmo::RuntimeInit) -> istmo::RuntimeInit {
    #[cfg(target_os = "android")]
    android_logger::init_once(
        android_logger::Config::default().with_max_level(log::LevelFilter::Info),
    );
    let typed: Arc<dyn EchoTyped> = Arc::new(TypedEcho);
    let init = init
        .expects::<KotlinBenchClient>()
        .host(RustBenchHost::new(RustBenchImpl))
        .host(EchoDispatch {
            plugin_id: ID_ISOLATED,
            mode: DispatchMode::Isolated,
        })
        .host(EchoDispatch {
            plugin_id: ID_INLINE,
            mode: DispatchMode::Inline,
        })
        .host(EchoDispatch {
            plugin_id: ID_TYPED,
            mode: DispatchMode::Inline,
        });
    init.runtime
        .register_typed_host::<dyn EchoTyped>(ID_TYPED, typed);
    init.finish()
}

// `istmo-android` pulls `android_activity`, which expects the app to
// export `android_main`. The bench runs a plain Java `Activity` and never
// enters it, but dlopen refuses an unresolved reference.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub const extern "C" fn android_main() {}

/// The running runtime (started by Kotlin's `IstmoRuntime`).
///
/// # Errors
///
/// The runtime has not been started.
pub fn runtime() -> Result<Arc<Runtime>, istmo::IstmoError> {
    Runtime::global()
}

/// Legacy batch mode: `iters` calls through `mode` (0 Isolated, 1 Inline,
/// 2 Typed), total ns. Kept so results compare with earlier runs.
///
/// # Errors
///
/// The runtime refused a call.
pub fn run_iterations(mode: i32, iters: u64, payload_len: usize) -> Result<u64, istmo::IstmoError> {
    let rt = runtime()?;
    let payload: Vec<u8> = (0..payload_len).map(|i| i.to_le_bytes()[0]).collect();
    let start = std::time::Instant::now();
    match mode {
        0 | 1 => {
            let plugin_id = if mode == 0 { ID_ISOLATED } else { ID_INLINE };
            for _ in 0..iters {
                let reply = rt
                    .call(plugin_id, None, "echo", payload.clone())?
                    .recv_blocking()?;
                let _ = std::hint::black_box(reply);
            }
        }
        _ => {
            let typed = rt
                .typed_host::<dyn EchoTyped>(ID_TYPED)
                .ok_or(istmo::IstmoError::ChannelClosed)?;
            for _ in 0..iters {
                std::hint::black_box(typed.echo(payload.clone()));
            }
        }
    }
    Ok(u64::try_from(start.elapsed().as_nanos()).unwrap_or(u64::MAX))
}
