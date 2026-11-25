//! Benchmark cells Rust drives: each returns one sample per iteration
//! (warm-up included; Kotlin discards it) plus a few cell-specific
//! extras.

use std::sync::Arc;
use std::time::Duration;

use istmo::{IstmoError, NativeFrame, Runtime, SinkPolicy, StreamItem};

use crate::contracts::{FrameMeta, KotlinBenchClient};
use crate::hosts::{EchoTyped, ID_INLINE, ID_ISOLATED, ID_TYPED};
use crate::sys::now_ns;
use crate::wake;

/// A Rust-driven cell. Ids mirror `BenchNative.Cell` in Kotlin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cell {
    /// Rust → Rust host on a fresh thread per call (`DispatchMode::Isolated`).
    IsolatedRaw,
    /// Rust → Rust host polled on the caller (`DispatchMode::Inline`).
    InlineRaw,
    /// Rust → typed shortcut (a plain trait call).
    Typed,
    /// Rust → Kotlin through the generated client (encode + decode).
    KotlinEcho,
    /// Rust → Kotlin with pre-encoded bytes (`Runtime::call`).
    KotlinEchoRaw,
    /// Kotlin-produced stream, paced: per-item emit → receive.
    KotlinStreamPaced,
    /// Kotlin-produced stream, back to back: per-item emit → receive,
    /// extra[0] = whole-stream ns.
    KotlinStreamBurst,
    /// flume bounded(1) round trip between two threads.
    FlumePingPong,
    /// Mutex + Condvar round trip.
    CondvarPingPong,
    /// Raw futex round trip.
    FutexPingPong,
    /// flume round trip spinning `size` µs before parking.
    SpinThenPark,
    /// Named thread spawn → body running.
    ThreadSpawnStart,
    /// Named thread spawn → joined.
    ThreadSpawnJoin,
    /// Two back-to-back clock reads.
    ClockOverhead,
}

impl TryFrom<i32> for Cell {
    type Error = i32;

    fn try_from(id: i32) -> Result<Self, i32> {
        Ok(match id {
            0 => Self::IsolatedRaw,
            1 => Self::InlineRaw,
            2 => Self::Typed,
            3 => Self::KotlinEcho,
            4 => Self::KotlinEchoRaw,
            5 => Self::KotlinStreamPaced,
            6 => Self::KotlinStreamBurst,
            12 => Self::FlumePingPong,
            13 => Self::CondvarPingPong,
            14 => Self::FutexPingPong,
            15 => Self::SpinThenPark,
            16 => Self::ThreadSpawnStart,
            17 => Self::ThreadSpawnJoin,
            18 => Self::ClockOverhead,
            other => return Err(other),
        })
    }
}

/// Samples plus cell-specific extras.
#[derive(Debug, Default)]
pub struct CellRun {
    pub samples: Vec<u64>,
    pub extra: [u64; 4],
}

impl Cell {
    /// Run `samples` iterations with a payload of `size` bytes.
    ///
    /// # Errors
    ///
    /// The runtime refused a call.
    pub fn run(
        self,
        rt: &Arc<Runtime>,
        samples: usize,
        size: usize,
    ) -> Result<CellRun, IstmoError> {
        let payload: Vec<u8> = (0..size).map(|i| i.to_le_bytes()[0]).collect();
        let samples = match self {
            Self::IsolatedRaw => raw_calls(rt, ID_ISOLATED, &payload, samples)?,
            Self::InlineRaw => raw_calls(rt, ID_INLINE, &payload, samples)?,
            Self::KotlinEchoRaw => {
                let encoded = istmo::codec::encode(&payload)?;
                raw_calls(
                    rt,
                    <KotlinBenchClient as istmo::Plugin>::PLUGIN_ID,
                    &encoded,
                    samples,
                )?
            }
            Self::Typed => {
                let typed = rt
                    .typed_host::<dyn EchoTyped>(ID_TYPED)
                    .ok_or(IstmoError::ChannelClosed)?;
                (0..samples)
                    .map(|_| {
                        let t0 = now_ns();
                        let out = typed.echo(payload.clone());
                        std::hint::black_box(out);
                        now_ns() - t0
                    })
                    .collect()
            }
            Self::KotlinEcho => {
                let client = KotlinBenchClient::from_runtime(rt)?;
                let mut out = Vec::with_capacity(samples);
                for _ in 0..samples {
                    let t0 = now_ns();
                    let reply = pollster::block_on(client.echo(payload.clone()))?;
                    let _ = std::hint::black_box(reply);
                    out.push(now_ns() - t0);
                }
                out
            }
            Self::KotlinStreamPaced => return kotlin_stream(rt, samples, size, 200),
            Self::KotlinStreamBurst => return kotlin_stream(rt, samples, size, 0),
            Self::FlumePingPong => wake::flume_ping_pong(samples, 0),
            Self::SpinThenPark => wake::flume_ping_pong(samples, size as u64),
            Self::CondvarPingPong => wake::condvar_ping_pong(samples),
            Self::FutexPingPong => wake::futex_ping_pong(samples),
            Self::ThreadSpawnStart => wake::thread_spawn(samples).0,
            Self::ThreadSpawnJoin => wake::thread_spawn(samples).1,
            Self::ClockOverhead => wake::clock_overhead(samples),
        };
        Ok(CellRun {
            samples,
            extra: [0; 4],
        })
    }
}

fn raw_calls(
    rt: &Arc<Runtime>,
    plugin: &str,
    payload: &[u8],
    samples: usize,
) -> Result<Vec<u64>, IstmoError> {
    let mut out = Vec::with_capacity(samples);
    for _ in 0..samples {
        let t0 = now_ns();
        let handle = rt.call(plugin, None, "echo", payload.to_vec())?;
        let reply = handle.recv_blocking()?;
        let _ = std::hint::black_box(reply);
        out.push(now_ns() - t0);
    }
    Ok(out)
}

fn kotlin_stream(
    rt: &Arc<Runtime>,
    samples: usize,
    size: usize,
    gap_us: u32,
) -> Result<CellRun, IstmoError> {
    let client = KotlinBenchClient::from_runtime(rt)?;
    let count = u32::try_from(samples).unwrap_or(u32::MAX);
    let size = u32::try_from(size.max(8)).unwrap_or(u32::MAX);
    let t0 = now_ns();
    let stream = client.ticks(count, size, gap_us)?;
    let mut out = Vec::with_capacity(samples);
    let mut first_item_ns = 0;
    while let StreamItem::Event(item) = stream.recv()? {
        let now = now_ns();
        if first_item_ns == 0 {
            first_item_ns = now - t0;
        }
        let sent = item
            .get(..8)
            .and_then(|b| b.try_into().ok())
            .map_or(now, u64::from_le_bytes);
        out.push(now.saturating_sub(sent));
    }
    let total = now_ns() - t0;
    Ok(CellRun {
        samples: out,
        extra: [total, first_item_ns, 0, 0],
    })
}

/// How a Kotlin producer hands frames over. Mirrors `FramePath` in Kotlin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FramePath {
    /// `submitBytes`: one copy.
    Bytes = 0,
    /// Slot pool: Rust-owned memory, no copy.
    Slot = 1,
    /// `submit(HardwareBuffer, meta, owner)`.
    HardwareBuffer = 2,
    /// `submit(Image)` from an `ImageReader`.
    Image = 3,
}

/// Per-frame measurements of one frame cell.
#[derive(Debug, Default)]
pub struct FrameRun {
    /// Kotlin submit start → Rust `recv` returned, by sequence number.
    pub latency: Vec<u64>,
    /// Rust dropped the frame → Kotlin owner closed (0: not measured).
    pub release: Vec<u64>,
    /// Time inside the Kotlin submit call.
    pub submit_cost: Vec<u64>,
    /// `[received, evicted, wrong buffer kind or size, not taken by the sink]`.
    pub counts: [u64; 4],
    /// End reason, pending fences, producer note.
    pub note: String,
}

/// Have Kotlin push `count` frames of `size` bytes through `path` into
/// a fresh sink and measure each hop.
///
/// # Errors
///
/// The sink could not open or the call failed.
pub fn run_frames(
    rt: &Arc<Runtime>,
    path: FramePath,
    count: u32,
    size: u32,
    gap_us: u32,
) -> Result<FrameRun, IstmoError> {
    let client = KotlinBenchClient::from_runtime(rt)?;
    let sink = Arc::new(rt.open_frame_sink::<FrameMeta>(SinkPolicy::queue(4))?);
    let id = sink.id();
    let n = usize::try_from(count).unwrap_or(0);
    let expected_len = usize::try_from(size).unwrap_or(0);
    let consumer_sink = Arc::clone(&sink);
    let consumer = std::thread::Builder::new()
        .name("bench-frames".to_owned())
        .spawn(move || {
            let mut latency = vec![0u64; n];
            let mut dropped_at = vec![0u64; n];
            let mut counts = [0u64; 4];
            while let Some(frame) = consumer_sink.recv() {
                let now = now_ns();
                counts[0] += 1;
                counts[1] += frame.dropped_before();
                if !frame_is_sound(&frame, path, expected_len) {
                    counts[2] += 1;
                }
                let seq = usize::try_from(frame.meta.seq).unwrap_or(usize::MAX);
                let submitted = frame.meta.submitted_ns;
                let at = now_ns();
                drop(frame);
                if let (Some(l), Some(d)) = (latency.get_mut(seq), dropped_at.get_mut(seq)) {
                    *l = now.saturating_sub(submitted);
                    *d = at;
                }
            }
            (latency, dropped_at, counts, consumer_sink.end_reason())
        })
        .map_err(|_| IstmoError::ChannelClosed)?;
    let report = pollster::block_on(client.frames(path as u32, count, size, gap_us, id));
    // A producer that failed without closing its sender would leave the
    // consumer waiting: give it a moment, then end the sink from here.
    let deadline = now_ns() + 5_000_000_000;
    while !consumer.is_finished() && now_ns() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    sink.close();
    let (latency, dropped_at, mut counts, end) =
        consumer.join().map_err(|_| IstmoError::ChannelClosed)?;
    let report = report?;
    counts[3] = u64::from(report.not_taken);
    let release = report
        .released_ns
        .iter()
        .zip(&dropped_at)
        .map(|(&released, &dropped)| {
            if released == 0 || dropped == 0 {
                0
            } else {
                released.saturating_sub(dropped)
            }
        })
        .collect();
    Ok(FrameRun {
        latency,
        release,
        submit_cost: report.submit_cost_ns,
        counts,
        note: format!(
            "end={end:?} first_refusal={} pending_fences={} note={}",
            report.first_refusal, report.pending_fences, report.note
        ),
    })
}

fn frame_is_sound(frame: &NativeFrame<FrameMeta>, path: FramePath, len: usize) -> bool {
    match path {
        FramePath::Bytes | FramePath::Slot => {
            frame.buffer.as_bytes().is_some_and(|b| b.len() == len)
        }
        FramePath::HardwareBuffer | FramePath::Image => frame.buffer.platform_ptr().is_some(),
    }
}
