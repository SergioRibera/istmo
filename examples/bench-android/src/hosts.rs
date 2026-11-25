//! Rust-side hosts: hand-written echo dispatchers in each
//! [`DispatchMode`], a typed shortcut, and the generated `RustBench`
//! plugin's implementation.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use istmo::{CancelToken, Dispatch, DispatchFuture, DispatchMode, InstanceId, Outcome};

use crate::contracts::RustBench;
use crate::sys::now_ns;

pub const ID_ISOLATED: &str = "bench.isolated";
pub const ID_INLINE: &str = "bench.inline";
pub const ID_TYPED: &str = "bench.typed";

/// Echo with no codec: the reply is the request bytes.
#[derive(Debug)]
pub struct EchoDispatch {
    pub plugin_id: &'static str,
    pub mode: DispatchMode,
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

/// Typed shortcut trait for the legacy typed cell.
pub trait EchoTyped: Send + Sync + 'static {
    fn echo(&self, bytes: Vec<u8>) -> Vec<u8>;
}

#[derive(Debug)]
pub struct TypedEcho;

impl EchoTyped for TypedEcho {
    fn echo(&self, bytes: Vec<u8>) -> Vec<u8> {
        bytes
    }
}

/// When set, the `RustBench` host stamps its body into [`HOST_STAMPS`].
pub static PROBING: AtomicBool = AtomicBool::new(false);
/// `[body entered, body returned]` of the last probed host call.
pub static HOST_STAMPS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];

#[derive(Debug, Default)]
pub struct RustBenchImpl;

impl RustBench for RustBenchImpl {
    async fn echo(&self, payload: Vec<u8>) -> Vec<u8> {
        if PROBING.load(Ordering::Relaxed) {
            HOST_STAMPS[0].store(now_ns(), Ordering::Relaxed);
            HOST_STAMPS[1].store(now_ns(), Ordering::Relaxed);
        }
        payload
    }

    fn ticks(&self, count: u32, size: u32, gap_us: u32) -> flume::Receiver<Vec<u8>> {
        let (tx, rx) = flume::bounded(64);
        let size = usize::try_from(size).unwrap_or(8).max(8);
        let gap = std::time::Duration::from_micros(u64::from(gap_us));
        let spawned = std::thread::Builder::new()
            .name("bench-rs-ticks".to_owned())
            .spawn(move || {
                for _ in 0..count {
                    if !gap.is_zero() {
                        std::thread::sleep(gap);
                    }
                    let mut item = vec![0u8; size];
                    item[..8].copy_from_slice(&now_ns().to_le_bytes());
                    if tx.send(item).is_err() {
                        return;
                    }
                }
            });
        if let Err(err) = spawned {
            tracing::error!(?err, "ticks producer thread");
        }
        rx
    }
}
