//! `dev.istmo.bench.BenchNative` entry points.
//!
//! Failures are reported as negative counts or empty strings rather than
//! exceptions: the Kotlin driver prints them next to the cell.

#![allow(
    unreachable_pub,
    reason = "exported to the JVM by `no_mangle`, not to Rust callers"
)]

use std::sync::atomic::Ordering;

use istmo::android::latency_probe::{self, Stage};
use jni::JNIEnv;
use jni::objects::{JByteArray, JClass, JLongArray, JString};
use jni::signature::{Primitive, ReturnType};
use jni::sys::{jboolean, jint, jlong, jvalue};

use crate::cells::{Cell, FramePath, run_frames};
use crate::hosts::{HOST_STAMPS, PROBING};
use crate::sys::{self, CpuMask, now_ns};

fn to_jlongs(values: &[u64]) -> Vec<jlong> {
    values
        .iter()
        .map(|&v| i64::try_from(v).unwrap_or(i64::MAX))
        .collect()
}

fn fill(env: &JNIEnv<'_>, out: &JLongArray<'_>, values: &[u64]) -> jint {
    let capacity = env.get_array_length(out).unwrap_or(0);
    let take = values.len().min(usize::try_from(capacity).unwrap_or(0));
    if env
        .set_long_array_region(out, 0, &to_jlongs(&values[..take]))
        .is_err()
    {
        return -2;
    }
    jint::try_from(take).unwrap_or(jint::MAX)
}

fn new_string<'local>(env: &JNIEnv<'local>, text: &str) -> JString<'local> {
    env.new_string(text).unwrap_or_default()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_nowNs(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jlong {
    i64::try_from(now_ns()).unwrap_or(i64::MAX)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_currentCpu(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jint {
    sys::current_cpu()
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_threadMap<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    new_string(&env, &sys::thread_map())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_cpuFreqs<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    new_string(&env, &sys::cpu_freqs())
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_pinProcess(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    mask: jlong,
) -> jint {
    jint::try_from(CpuMask(mask.cast_unsigned()).apply_to_process()).unwrap_or(jint::MAX)
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_startSpinners(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    count: jint,
    mask: jlong,
) {
    sys::start_spinners(
        u32::try_from(count).unwrap_or(0),
        CpuMask(mask.cast_unsigned()),
    );
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_stopSpinners(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) {
    sys::stop_spinners();
}

/// Run a Rust-driven cell; fills `out` with samples and `extra` with
/// cell extras. Returns the sample count, or a negative code.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_runCell<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    cell: jint,
    samples: jint,
    size: jint,
    out: JLongArray<'local>,
    extra: JLongArray<'local>,
) -> jint {
    let Ok(cell) = Cell::try_from(cell) else {
        return -1;
    };
    let Ok(rt) = crate::runtime() else { return -3 };
    let samples = usize::try_from(samples).unwrap_or(0);
    let size = usize::try_from(size).unwrap_or(0);
    match cell.run(&rt, samples, size) {
        Ok(run) => {
            let _ = fill(&env, &extra, &run.extra);
            fill(&env, &out, &run.samples)
        }
        Err(err) => {
            log::error!("cell {cell:?} failed: {err}");
            -4
        }
    }
}

/// Run a frame-sink cell. Returns the producer/consumer note.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_runFrames<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: jint,
    count: jint,
    size: jint,
    gap_us: jint,
    latency: JLongArray<'local>,
    release: JLongArray<'local>,
    submit: JLongArray<'local>,
    counts: JLongArray<'local>,
) -> JString<'local> {
    let path = match path {
        0 => FramePath::Bytes,
        1 => FramePath::Slot,
        2 => FramePath::HardwareBuffer,
        _ => FramePath::Image,
    };
    let Ok(rt) = crate::runtime() else {
        return new_string(&env, "runtime not started");
    };
    let as_u32 = |v: jint| u32::try_from(v).unwrap_or(0);
    match run_frames(&rt, path, as_u32(count), as_u32(size), as_u32(gap_us)) {
        Ok(run) => {
            let _ = fill(&env, &latency, &run.latency);
            let _ = fill(&env, &release, &run.release);
            let _ = fill(&env, &submit, &run.submit_cost);
            let _ = fill(&env, &counts, &run.counts);
            new_string(&env, &run.note)
        }
        Err(err) => new_string(&env, &format!("error: {err}")),
    }
}

/// Rust → Kotlin JNI baselines on the calling (attached) thread:
/// `kind` 0 = `CallStaticVoidMethod` on `kNoop()`, 1 = new `byte[size]`
/// + `kNoopBytes(byte[])`, 2 = attach + detach a fresh native thread.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_jniCallbacks<'local>(
    mut env: JNIEnv<'local>,
    class: JClass<'local>,
    kind: jint,
    samples: jint,
    size: jint,
    out: JLongArray<'local>,
) -> jint {
    let samples = usize::try_from(samples).unwrap_or(0);
    let payload = vec![7u8; usize::try_from(size).unwrap_or(0)];
    let result = match kind {
        0 | 1 => callbacks(&mut env, &class, kind == 1, &payload, samples),
        _ => attach_detach(&env, samples),
    };
    match result {
        Ok(values) => fill(&env, &out, &values),
        Err(err) => {
            log::error!("jni callbacks failed: {err}");
            let _ = env.exception_clear();
            -4
        }
    }
}

fn callbacks(
    env: &mut JNIEnv<'_>,
    class: &JClass<'_>,
    with_bytes: bool,
    payload: &[u8],
    samples: usize,
) -> Result<Vec<u64>, jni::errors::Error> {
    let (name, sig) = if with_bytes {
        ("kNoopBytes", "([B)V")
    } else {
        ("kNoop", "()V")
    };
    let method = env.get_static_method_id(class, name, sig)?;
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = now_ns();
        env.with_local_frame(4, |env| {
            let args: Vec<jvalue> = if with_bytes {
                let bytes = env.byte_array_from_slice(payload)?;
                vec![jni::objects::JValue::Object(&bytes).as_jni()]
            } else {
                Vec::new()
            };
            // SAFETY: `method` was resolved on `class` with signature
            // `sig`, and `args` matches it: none for `()V`, one byte
            // array for `([B)V`; the return type is void for both.
            unsafe {
                env.call_static_method_unchecked(
                    class,
                    method,
                    ReturnType::Primitive(Primitive::Void),
                    &args,
                )
            }?;
            Ok::<(), jni::errors::Error>(())
        })?;
        out.push(now_ns() - t0);
    }
    Ok(out)
}

fn attach_detach(env: &JNIEnv<'_>, samples: usize) -> Result<Vec<u64>, jni::errors::Error> {
    let vm = env.get_java_vm()?;
    let handle = std::thread::Builder::new()
        .name("bench-attach".to_owned())
        .spawn(move || {
            (0..samples)
                .map(|_| {
                    let t0 = now_ns();
                    let guard = vm.attach_current_thread();
                    drop(guard);
                    now_ns() - t0
                })
                .collect::<Vec<u64>>()
        })
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::Unknown))?;
    handle
        .join()
        .map_err(|_| jni::errors::Error::JniCall(jni::errors::JniError::Unknown))
}

/// Legacy batch mode (see `run_iterations`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_runIters(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    mode: jint,
    iters: jlong,
    payload_len: jint,
) -> jlong {
    let iters = u64::try_from(iters).unwrap_or(0);
    let len = usize::try_from(payload_len).unwrap_or(0);
    crate::run_iterations(mode, iters, len).map_or(-1, |ns| i64::try_from(ns).unwrap_or(i64::MAX))
}

/// Bare Kotlin → Rust JNI entry and exit.
#[unsafe(no_mangle)]
pub const extern "system" fn Java_dev_istmo_bench_BenchNative_noopJni(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
) -> jlong {
    0
}

/// Kotlin → Rust JNI entry plus `convert_byte_array` (pin + copy).
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_noopJniBytes<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    payload: JByteArray<'local>,
) -> jlong {
    let _ = std::hint::black_box(env.convert_byte_array(&payload));
    0
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_setProbing(
    _env: JNIEnv<'_>,
    _class: JClass<'_>,
    on: jboolean,
) {
    PROBING.store(on != 0, Ordering::Relaxed);
}

/// Index of every transport stamp in the arrays below.
const PROBE_STAGES: [Stage; Stage::COUNT] = [
    Stage::PumpDequeued,
    Stage::PumpCallingKotlin,
    Stage::PumpKotlinReturned,
    Stage::SubmitResponseEntered,
    Stage::SubmitResponseDispatched,
    Stage::SubmitCallEntered,
    Stage::SubmitCallDispatched,
    Stage::SubmitEventEntered,
    Stage::DirectCallingKotlin,
    Stage::DirectKotlinReturned,
];

/// Wait (up to 2 ms) until `after` is at least as recent as `before`.
fn settle_after(before: Stage, after: Stage) {
    let deadline = now_ns() + 2_000_000;
    while latency_probe::read(after) < latency_probe::read(before) && now_ns() < deadline {
        std::hint::spin_loop();
    }
}

fn transport_stamps() -> Vec<u64> {
    PROBE_STAGES
        .iter()
        .map(|&stage| latency_probe::read(stage))
        .collect()
}

/// One probed Rust → Kotlin → Rust echo. `out` = `[before encode, after
/// encode, call queued, reply received, reply decoded]` followed by the
/// transport stamps.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_probeRustToKotlin<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    size: jint,
    out: JLongArray<'local>,
) -> jint {
    let Ok(rt) = crate::runtime() else { return -3 };
    let payload = vec![3u8; usize::try_from(size).unwrap_or(0)];
    let plugin = <crate::contracts::KotlinBenchClient as istmo::Plugin>::PLUGIN_ID;
    let r0 = now_ns();
    let Ok(encoded) = istmo::codec::encode(&payload) else {
        return -4;
    };
    let r1 = now_ns();
    let Ok(handle) = rt.call(plugin, None, "echo", encoded) else {
        return -4;
    };
    let r2 = now_ns();
    let Ok(istmo::CallResult::Ok(reply)) = handle.recv_blocking() else {
        return -5;
    };
    let r3 = now_ns();
    let Ok(decoded) = istmo::codec::decode_exact::<Vec<u8>>(&reply) else {
        return -6;
    };
    let r4 = now_ns();
    std::hint::black_box(decoded);
    // The pump and the Kotlin thread stamp their "after" stages once the
    // hand-off is done, which can be after this caller woke: wait for them.
    settle_after(
        Stage::SubmitResponseEntered,
        Stage::SubmitResponseDispatched,
    );
    settle_after(Stage::PumpCallingKotlin, Stage::PumpKotlinReturned);
    let mut stamps = vec![r0, r1, r2, r3, r4];
    stamps.extend(transport_stamps());
    fill(&env, &out, &stamps)
}

/// The last probed Rust host body (`[entered, returned]`) followed by
/// the transport stamps.
#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_istmo_bench_BenchNative_readStamps<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    out: JLongArray<'local>,
) -> jint {
    settle_after(Stage::PumpCallingKotlin, Stage::PumpKotlinReturned);
    let mut stamps = vec![
        HOST_STAMPS[0].load(Ordering::Relaxed),
        HOST_STAMPS[1].load(Ordering::Relaxed),
    ];
    stamps.extend(transport_stamps());
    fill(&env, &out, &stamps)
}
