//! The two plugins the benchmark crosses the boundary with: one Kotlin
//! implements and Rust calls, one Rust implements and Kotlin calls.
//!
//! `build.rs` extracts both contracts from this file to generate the
//! Kotlin dispatcher and client, so keep it free of anything else.

use istmo::{FrameSinkId, message, plugin};

/// Metadata of one benchmark frame: its sequence number and the
/// `CLOCK_MONOTONIC` time the producer started submitting it.
#[message]
#[derive(Debug, Clone)]
pub struct FrameMeta {
    pub seq: u32,
    pub submitted_ns: u64,
}

/// What a Kotlin frame producer measured on its side.
#[message]
#[derive(Debug, Clone, Default)]
pub struct FrameReport {
    /// Time spent inside each submit call, by sequence number.
    pub submit_cost_ns: Vec<u64>,
    /// When the producer got each frame's buffer back (`0`: unknown), by
    /// sequence number.
    pub released_ns: Vec<u64>,
    /// Submits the sink did not take.
    pub not_taken: u32,
    /// First status the sink answered with that was not a take.
    pub first_refusal: u32,
    /// Images whose acquire fence was still pending at submit (API 33+).
    pub pending_fences: u32,
    /// Free-form note (unsupported path, exception text).
    pub note: String,
}

/// Implemented in Kotlin (`KotlinBenchBackend`), called from Rust.
#[plugin(name = "bench.kotlin")]
pub trait KotlinBench {
    /// Return `payload` unchanged.
    async fn echo(&self, payload: Vec<u8>) -> Vec<u8>;

    /// Emit `count` items of `size` bytes, `gap_us` apart; each item
    /// starts with the emitter's `System.nanoTime()` (little endian).
    #[istmo::stream]
    fn ticks(&self, count: u32, size: u32, gap_us: u32) -> Vec<u8>;

    /// Submit `count` frames of `size` bytes through `path` (see
    /// `FramePath`), `gap_us` apart, into `frames`.
    async fn frames(
        &self,
        path: u32,
        count: u32,
        size: u32,
        gap_us: u32,
        frames: FrameSinkId<FrameMeta>,
    ) -> FrameReport;
}

/// Implemented in Rust (`RustBenchImpl`), called from Kotlin.
#[plugin(name = "bench.rust")]
pub trait RustBench {
    /// Return `payload` unchanged.
    async fn echo(&self, payload: Vec<u8>) -> Vec<u8>;

    /// Emit `count` items of `size` bytes, `gap_us` apart; each item
    /// starts with the producer's `CLOCK_MONOTONIC` (little endian).
    #[istmo::stream]
    fn ticks(&self, count: u32, size: u32, gap_us: u32) -> Vec<u8>;
}
