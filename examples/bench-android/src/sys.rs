//! Clock, CPU placement and load helpers the benchmark cells share.

use std::fmt::Write as _;
use std::hint::black_box;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

/// `CLOCK_MONOTONIC` in nanoseconds: the clock `System.nanoTime()`
/// reads on Android, so stamps from both languages subtract directly.
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

/// The CPU the calling thread runs on (`-1` when unknown).
#[must_use]
pub fn current_cpu() -> i32 {
    // SAFETY: `sched_getcpu` takes no arguments and only reads kernel state.
    unsafe { libc::sched_getcpu() }
}

/// A set of CPUs as a bit mask (bit `n` = CPU `n`); `0` means "all".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CpuMask(pub u64);

impl CpuMask {
    fn to_set(self) -> libc::cpu_set_t {
        // SAFETY: `cpu_set_t` is plain data; all-zero is the empty set.
        let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
        let bits = if self.0 == 0 { u64::MAX } else { self.0 };
        for cpu in 0..64usize {
            if bits & (1 << cpu) != 0 {
                // SAFETY: `cpu` < 64 fits the 1024-bit `cpu_set_t`.
                unsafe { libc::CPU_SET(cpu, &mut set) };
            }
        }
        set
    }

    /// Restrict thread `tid` (`0`: the caller) to this mask.
    ///
    /// # Errors
    ///
    /// The kernel refused (unknown thread, or no allowed CPU online).
    pub fn apply(self, tid: libc::pid_t) -> std::io::Result<()> {
        let set = self.to_set();
        // SAFETY: `set` is a valid `cpu_set_t` of the size passed.
        let rc =
            unsafe { libc::sched_setaffinity(tid, size_of::<libc::cpu_set_t>(), &raw const set) };
        if rc == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    }

    /// Restrict every thread of this process to the mask; returns how
    /// many threads took it. New threads inherit their creator's mask.
    #[must_use]
    pub fn apply_to_process(self) -> u32 {
        let mut pinned = 0;
        for tid in task_ids() {
            if self.apply(tid).is_ok() {
                pinned += 1;
            }
        }
        pinned
    }
}

fn task_ids() -> Vec<libc::pid_t> {
    std::fs::read_dir("/proc/self/task")
        .map(|dir| {
            dir.filter_map(Result::ok)
                .filter_map(|entry| entry.file_name().to_str()?.parse().ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Threads worth reporting, by the prefix of their (15-byte) name.
const WATCHED: [&str; 9] = [
    "istmo-android-p",
    "DefaultDispatch",
    "istmo-bench",
    "istmo-worker",
    "istmo-frame-rel",
    "bench-",
    "istmo-blocking",
    "main",
    "istmo-hosted",
];

/// `name@cpu` for every watched thread, from `/proc/self/task/*/stat`
/// (field 39: the CPU the thread last ran on).
#[must_use]
pub fn thread_map() -> String {
    let mut seen: Vec<(String, i64)> = Vec::new();
    for tid in task_ids() {
        let Ok(stat) = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")) else {
            continue;
        };
        let (Some(open), Some(close)) = (stat.find('('), stat.rfind(')')) else {
            continue;
        };
        let name = stat[open + 1..close].replace(' ', "_");
        if !WATCHED.iter().any(|prefix| name.starts_with(prefix)) {
            continue;
        }
        // Fields after the name start at field 3 (state); 39 is index 36.
        let cpu = stat[close + 1..]
            .split_whitespace()
            .nth(36)
            .and_then(|field| field.parse().ok())
            .unwrap_or(-1);
        if !seen.iter().any(|(n, c)| *n == name && *c == cpu) {
            seen.push((name, cpu));
        }
    }
    seen.sort();
    let mut out = String::new();
    for (name, cpu) in seen {
        let _ = write!(out, "{name}@{cpu} ");
    }
    out.trim_end().to_owned()
}

/// Current frequency (kHz) of every CPU, `-` when unreadable.
#[must_use]
pub fn cpu_freqs() -> String {
    let mut out = String::new();
    for cpu in 0..16 {
        let base = format!("/sys/devices/system/cpu/cpu{cpu}/cpufreq");
        if std::fs::metadata(&base).is_err() {
            break;
        }
        let freq = std::fs::read_to_string(format!("{base}/scaling_cur_freq"))
            .map_or_else(|_| "-".to_owned(), |f| f.trim().to_owned());
        let _ = write!(out, "{cpu}:{freq} ");
    }
    out.trim_end().to_owned()
}

static SPIN_STOP: AtomicBool = AtomicBool::new(false);
static SPINNERS: Mutex<Vec<JoinHandle<()>>> = Mutex::new(Vec::new());

/// Background load: `count` threads spinning on `mask` until
/// [`stop_spinners`].
pub fn start_spinners(count: u32, mask: CpuMask) {
    SPIN_STOP.store(false, Ordering::Relaxed);
    let mut spinners = SPINNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    for n in 0..count {
        let spawned = std::thread::Builder::new()
            .name(format!("bench-spin-{n}"))
            .spawn(move || {
                let _ = mask.apply(0);
                let mut x = 0u64;
                while !SPIN_STOP.load(Ordering::Relaxed) {
                    x = black_box(x.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1));
                }
            });
        if let Ok(handle) = spawned {
            spinners.push(handle);
        }
    }
}

/// Stop and join every spinner.
pub fn stop_spinners() {
    SPIN_STOP.store(true, Ordering::Relaxed);
    let handles: Vec<_> = SPINNERS
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .drain(..)
        .collect();
    for handle in handles {
        let _ = handle.join();
    }
}
