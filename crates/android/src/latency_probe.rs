//! `CLOCK_MONOTONIC` stamps at the transport's hand-off points, for
//! on-device latency breakdowns. Compiled only with the off-by-default
//! `latency-probe` feature.
//!
//! Each [`Stage`] keeps the time it was last reached, so a benchmark
//! driving one call at a time reads one stamp per stage and call. The
//! clock is the one `System.nanoTime()` reads on Android, so Kotlin and
//! Rust stamps subtract directly.

use std::sync::atomic::{AtomicU64, Ordering};

/// A hand-off point inside the transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(usize)]
pub enum Stage {
    /// The pump's selector returned an outbound frame.
    PumpDequeued = 0,
    /// The pump built the JNI arguments and is about to call Kotlin.
    PumpCallingKotlin = 1,
    /// The Kotlin callback returned to the pump.
    PumpKotlinReturned = 2,
    /// `nativeSubmitResponse` entered Rust.
    SubmitResponseEntered = 3,
    /// `nativeSubmitResponse` handed the response to the waiter.
    SubmitResponseDispatched = 4,
    /// `nativeSubmitCall` entered Rust.
    SubmitCallEntered = 5,
    /// `nativeSubmitCall` dispatched the call to its host.
    SubmitCallDispatched = 6,
    /// `nativeSubmitEvent` entered Rust.
    SubmitEventEntered = 7,
    /// A direct Rust → Kotlin call built its arguments and is about to
    /// call `onCallDirect` on the caller's thread.
    DirectCallingKotlin = 8,
    /// `onCallDirect` returned to the direct caller.
    DirectKotlinReturned = 9,
}

impl Stage {
    /// Number of stages.
    pub const COUNT: usize = 10;
}

static STAMPS: [AtomicU64; Stage::COUNT] = [const { AtomicU64::new(0) }; Stage::COUNT];

/// `CLOCK_MONOTONIC` in nanoseconds.
#[must_use]
pub fn now_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid, writable `timespec`; CLOCK_MONOTONIC is
    // always supported, so the call cannot fail.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &raw mut ts) };
    u64::try_from(ts.tv_sec).unwrap_or(0) * 1_000_000_000 + u64::try_from(ts.tv_nsec).unwrap_or(0)
}

/// Record that `stage` was reached now.
pub fn stamp(stage: Stage) {
    STAMPS[stage as usize].store(now_ns(), Ordering::Relaxed);
}

/// When `stage` was last reached (`0`: never).
#[must_use]
pub fn read(stage: Stage) -> u64 {
    STAMPS[stage as usize].load(Ordering::Relaxed)
}

/// Every stage's last stamp, indexed by `Stage as usize`.
#[must_use]
pub fn snapshot() -> [u64; Stage::COUNT] {
    std::array::from_fn(|i| STAMPS[i].load(Ordering::Relaxed))
}
