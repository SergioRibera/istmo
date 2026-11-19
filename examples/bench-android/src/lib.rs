//! On-device istmo benchmark harness.
//!
//! Boots a runtime with three identical echo hosts — one in each
//! [`DispatchMode`] — plus a typed shortcut for the inline host, and
//! exposes a single JNI entry point the Kotlin driver calls to run a
//! fixed number of iterations and report the elapsed time in
//! nanoseconds.
//!
//! The Kotlin side owns the histogram and the device matrix; this
//! crate only answers how long N calls through a given mode took.

#![allow(non_snake_case)]

use std::sync::Arc;
use std::sync::OnceLock;

use istmo::{
    CancelToken, Dispatch, DispatchFuture, DispatchMode, InstanceId, Outcome, Runtime,
    RuntimeConfig,
};

static STARTED: OnceLock<()> = OnceLock::new();

const ID_ISOLATED: &str = "bench.isolated";
const ID_INLINE: &str = "bench.inline";
const ID_TYPED: &str = "bench.typed";

/// Benchmark mode. Mirrors the Kotlin enum.
#[repr(u32)]
#[derive(Clone, Copy, Debug)]
enum Mode {
    Isolated = 0,
    Inline = 1,
    Typed = 2,
}

impl TryFrom<u32> for Mode {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Isolated),
            1 => Ok(Self::Inline),
            2 => Ok(Self::Typed),
            _ => Err(()),
        }
    }
}

struct EchoDispatch {
    plugin_id: &'static str,
    mode: DispatchMode,
}

impl Dispatch for EchoDispatch {
    fn plugin_id(&self) -> &'static str {
        self.plugin_id
    }

    fn mode(&self) -> DispatchMode {
        self.mode
    }

    fn dispatch<'a>(
        &'a self,
        _instance: Option<InstanceId>,
        _method: &'a str,
        payload: &'a [u8],
        _cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let out = payload.to_vec();
        Box::pin(async move { Ok(Outcome::Ok(out)) })
    }
}

/// Typed shortcut trait for the typed mode.
trait EchoTyped: Send + Sync + 'static {
    fn echo(&self, bytes: Vec<u8>) -> Vec<u8>;
}

struct TypedEcho;

impl EchoTyped for TypedEcho {
    fn echo(&self, bytes: Vec<u8>) -> Vec<u8> {
        bytes
    }
}

fn boot_runtime() -> Arc<Runtime> {
    STARTED.get_or_init(|| {
        #[cfg(target_os = "android")]
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );

        let init = Runtime::init(RuntimeConfig::inline()).expect("init runtime");
        let rt = init.runtime;

        rt.register_host(EchoDispatch {
            plugin_id: ID_ISOLATED,
            mode: DispatchMode::Isolated,
        });
        rt.register_host(EchoDispatch {
            plugin_id: ID_INLINE,
            mode: DispatchMode::Inline,
        });
        // Typed host: both wire-registered (as Inline) and typed-registered.
        rt.register_host(EchoDispatch {
            plugin_id: ID_TYPED,
            mode: DispatchMode::Inline,
        });
        let typed: Arc<dyn EchoTyped> = Arc::new(TypedEcho);
        rt.register_typed_host::<dyn EchoTyped>(ID_TYPED, typed);
    });
    Runtime::global().expect("runtime global")
}

fn run_iterations(mode: Mode, iters: u64, payload_len: usize) -> u64 {
    let rt = boot_runtime();
    let payload: Vec<u8> = (0..payload_len).map(|i| i as u8).collect();

    match mode {
        Mode::Isolated | Mode::Inline => {
            let plugin_id = match mode {
                Mode::Isolated => ID_ISOLATED,
                Mode::Inline => ID_INLINE,
                Mode::Typed => unreachable!(),
            };
            let start = std::time::Instant::now();
            for _ in 0..iters {
                let handle = rt
                    .call(plugin_id, None, "ping", payload.clone())
                    .expect("call");
                let _ = handle.recv_blocking();
            }
            start.elapsed().as_nanos() as u64
        }
        Mode::Typed => {
            let typed = rt
                .typed_host::<dyn EchoTyped>(ID_TYPED)
                .expect("typed registered");
            let start = std::time::Instant::now();
            for _ in 0..iters {
                let out = typed.echo(payload.clone());
                let _ = std::hint::black_box(out);
            }
            start.elapsed().as_nanos() as u64
        }
    }
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_runIters<'local>(
    _env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    mode: jni::sys::jint,
    iters: jni::sys::jlong,
    payload_len: jni::sys::jint,
) -> jni::sys::jlong {
    let mode = u32::try_from(mode).ok().and_then(|m| Mode::try_from(m).ok());
    let iters = u64::try_from(iters).unwrap_or(0);
    let payload_len = usize::try_from(payload_len).unwrap_or(0);
    let Some(mode) = mode else { return -1 };
    run_iterations(mode, iters, payload_len) as jni::sys::jlong
}

/// Baseline: pure JNI roundtrip. Kotlin loops and times this with
/// `System.nanoTime()`; the ns/call floor it reports is the raw JNI
/// machinery (method lookup, frame push/pop, no payload), which every
/// istmo-over-JNI call pays before any istmo logic runs.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_noopJni<'local>(
    _env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
) -> jni::sys::jlong {
    0
}

/// Baseline: JNI roundtrip + `ByteArray` decode. The gap between this
/// and [`noopJni`] is what [`convert_byte_array`] alone costs (JVM
/// pinning + the memcpy into a fresh `Vec<u8>`). The gap between this
/// and the full `runIters(Isolated, …)` is what istmo itself adds on
/// top of the JNI + payload floor.
#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_noopJniBytes<'local>(
    mut env: jni::JNIEnv<'local>,
    _class: jni::objects::JClass<'local>,
    payload: jni::objects::JByteArray<'local>,
) -> jni::sys::jlong {
    let _ = env.convert_byte_array(&payload);
    0
}

/// Desktop entry for a quick smoke check: runs each mode for 10 000
/// iterations of a 64-byte payload and prints the ns totals.
#[cfg(not(target_os = "android"))]
pub fn smoke() {
    for &mode in &[Mode::Isolated, Mode::Inline, Mode::Typed] {
        let ns = run_iterations(mode, 10_000, 64);
        println!("{mode:?}: {ns} ns total, {} ns/iter", ns / 10_000);
    }
}
