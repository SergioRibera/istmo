//! The blocking workers: persistent threads for hosted work that may
//! block ([`DispatchMode::Isolated`], `#[istmo::blocking]` methods).
//!
//! A job goes to an idle thread when there is one and to a new thread
//! otherwise, so no job ever waits behind busy ones: a host that blocks
//! waiting for another blocking call (however deep the chain) always
//! gets a thread for it, as with a thread per call. The first
//! [`kept_threads`] threads never exit; the others leave after
//! [`IDLE_EXIT`] without work.
//!
//! Each idle thread waits on a slot of its own. A job is handed to one
//! slot, never to a queue every thread reads, so the idle list is exact:
//! a thread is either running a job, listed idle and waiting on its
//! slot, or about to receive the job a submit just handed it.
//!
//! [`DispatchMode::Isolated`]: crate::DispatchMode::Isolated

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use flume::{Receiver, RecvTimeoutError, Sender};

use crate::sync::lock;

/// How long a thread beyond the kept ones waits for work before exiting.
const IDLE_EXIT: Duration = Duration::from_secs(30);

/// Threads alive above which the pool warns (once, until it shrinks
/// back below half): that many blocking calls at once usually means a
/// burst parked on one lock, or blocking calls waiting on each other.
const HIGH_WATER: usize = 256;

type Job = Box<dyn FnOnce() + Send + 'static>;

/// An idle thread: where to hand it a job, and who it is.
struct Idle {
    id: u64,
    slot: Sender<Job>,
}

#[derive(Default)]
struct Threads {
    /// Threads alive: running a job or idle.
    alive: usize,
    /// Idle threads, most recently idle last (reused first: its stack
    /// and caches are warm).
    idle: Vec<Idle>,
    /// Names the next thread.
    next_id: u64,
    /// Most threads ever alive at once.
    peak: usize,
    /// Warned about [`HIGH_WATER`] since the pool last shrank below half.
    warned: bool,
}

impl Threads {
    /// Take thread `id` off the idle list; `false` when it was not on it.
    fn unlist(&mut self, id: u64) -> bool {
        let Some(at) = self.idle.iter().position(|idle| idle.id == id) else {
            return false;
        };
        self.idle.swap_remove(at);
        true
    }
}

fn threads() -> &'static Mutex<Threads> {
    static THREADS: OnceLock<Mutex<Threads>> = OnceLock::new();
    THREADS.get_or_init(Mutex::default)
}

/// Threads that stay alive once started, so a steady caller never pays
/// for a thread start.
fn kept_threads() -> usize {
    std::thread::available_parallelism().map_or(2, std::num::NonZero::get)
}

/// Run `job` on a blocking thread: an idle one, or a new one.
///
/// # Errors
///
/// No thread was idle and the OS refused a new one; `job` is dropped
/// unrun.
pub(super) fn spawn(job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let mut job: Job = Box::new(job);
    loop {
        let mut threads = lock(threads());
        let Some(idle) = threads.idle.pop() else {
            threads.alive += 1;
            threads.peak = threads.peak.max(threads.alive);
            let alive = threads.alive;
            let warn = alive > HIGH_WATER && !threads.warned;
            threads.warned |= warn;
            let id = threads.next_id;
            threads.next_id += 1;
            drop(threads);
            if warn {
                tracing::warn!(
                    alive,
                    "over {HIGH_WATER} istmo blocking workers alive: that many blocking calls run at once"
                );
            }
            return start(id, job);
        };
        drop(threads);
        // A listed thread keeps its slot's receiver until it is off the
        // list, so this only fails for a thread that died listed; try
        // the next one.
        match idle.slot.send(job) {
            Ok(()) => return Ok(()),
            Err(flume::SendError(returned)) => job = returned,
        }
    }
}

fn start(id: u64, first: Job) -> std::io::Result<()> {
    let (slot, jobs) = flume::bounded::<Job>(1);
    let started = std::thread::Builder::new()
        .name(format!("istmo-blocking-{id}"))
        .spawn(move || {
            let _alive = Alive(id);
            run(first);
            worker_loop(id, &slot, &jobs);
        });
    started.map(drop).inspect_err(|_| {
        lock(threads()).alive -= 1;
    })
}

/// Keeps [`Threads`] right however the thread ends.
struct Alive(u64);

impl Drop for Alive {
    fn drop(&mut self) {
        let mut threads = lock(threads());
        threads.unlist(self.0);
        threads.alive -= 1;
        if threads.alive < HIGH_WATER / 2 {
            threads.warned = false;
        }
    }
}

/// Run one job; a panic (the job's own, or one while dropping what it
/// captured) must not kill the thread.
fn run(job: Job) {
    if catch_unwind(AssertUnwindSafe(job)).is_err() {
        tracing::error!("an istmo blocking job panicked");
    }
}

fn worker_loop(id: u64, slot: &Sender<Job>, jobs: &Receiver<Job>) {
    loop {
        lock(threads()).idle.push(Idle {
            id,
            slot: slot.clone(),
        });
        let job = loop {
            match jobs.recv_timeout(IDLE_EXIT) {
                Ok(job) => break job,
                Err(RecvTimeoutError::Timeout) => {
                    let mut threads = lock(threads());
                    // Still listed: nobody is handing it a job, so it may
                    // leave. Not listed: a submit took it and the job is
                    // on its way.
                    if threads.alive > kept_threads() && threads.unlist(id) {
                        return;
                    }
                }
                // Cannot happen while `slot` lives in this frame.
                Err(RecvTimeoutError::Disconnected) => return,
            }
        };
        run(job);
    }
}

/// Threads alive now, and the most ever alive at once.
pub(super) fn counts() -> (usize, usize) {
    let threads = lock(threads());
    (threads.alive, threads.peak)
}

/// Threads alive (tests).
#[cfg(test)]
pub(super) fn alive() -> usize {
    counts().0
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};

    use super::*;

    #[test]
    fn sequential_jobs_reuse_threads() {
        let (tx, rx) = flume::unbounded();
        let mut names = std::collections::HashSet::new();
        for _ in 0..50 {
            let tx = tx.clone();
            spawn(move || {
                tx.send(std::thread::current().name().map(str::to_owned))
                    .unwrap();
            })
            .unwrap();
            names.insert(rx.recv_timeout(Duration::from_secs(5)).unwrap());
            // Let the thread list itself idle again.
            std::thread::sleep(Duration::from_millis(1));
        }
        // Other tests share the pool, so only check that threads were
        // reused at all.
        assert!(names.len() < 50);
    }

    #[test]
    fn concurrent_blocking_jobs_each_get_a_thread() {
        // Eight jobs that only finish once all eight run at once.
        let count = 8;
        let barrier = Arc::new(Barrier::new(count + 1));
        for _ in 0..count {
            let barrier = Arc::clone(&barrier);
            spawn(move || {
                barrier.wait();
            })
            .unwrap();
        }
        barrier.wait();
    }

    /// A job that waits for a job it spawns, `depth` levels deep.
    fn nested(depth: usize, done: &Sender<()>) {
        if depth > 0 {
            let (tx, rx) = flume::bounded(1);
            spawn(move || nested(depth - 1, &tx)).unwrap();
            rx.recv().unwrap();
        }
        done.send(()).unwrap();
    }

    #[test]
    fn nested_blocking_jobs_deeper_than_any_cap_finish() {
        let (tx, rx) = flume::bounded(1);
        spawn(move || nested(200, &tx)).unwrap();
        rx.recv_timeout(Duration::from_secs(30))
            .expect("200 nested blocking jobs, each waiting on the next");
    }

    #[test]
    fn a_panicking_job_keeps_the_pool_serving() {
        for _ in 0..4 {
            spawn(|| panic!("job bug")).unwrap();
        }
        let (tx, rx) = flume::bounded(1);
        spawn(move || tx.send(()).unwrap()).unwrap();
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(alive() >= 1);
    }
}
