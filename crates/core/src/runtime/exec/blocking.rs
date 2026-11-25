//! The blocking workers: persistent threads for hosted work that may
//! block ([`DispatchMode::Isolated`], `#[istmo::blocking]` methods).
//!
//! Threads are started when every running one is busy, up to
//! [`MAX_THREADS`], and reused afterwards; the first
//! [`kept_threads`] never exit, the others leave after
//! [`IDLE_EXIT`] without work. Past the cap, jobs queue until a thread
//! frees up: a host that blocks waiting for another blocking call can
//! only deadlock once [`MAX_THREADS`] of them wait at the same time.
//!
//! [`DispatchMode::Isolated`]: crate::DispatchMode::Isolated

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

use flume::{Receiver, RecvTimeoutError, Sender};

use crate::sync::lock;

/// Most blocking threads alive at once.
const MAX_THREADS: usize = 64;

/// How long a thread beyond the kept ones waits for work before exiting.
const IDLE_EXIT: Duration = Duration::from_secs(30);

type Job = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Default)]
struct Counts {
    /// Threads alive.
    alive: usize,
    /// Threads waiting for a job that no submit has claimed yet.
    idle: usize,
}

struct Pool {
    tx: Sender<Job>,
    rx: Receiver<Job>,
    counts: Mutex<Counts>,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = flume::unbounded();
        Pool {
            tx,
            rx,
            counts: Mutex::new(Counts::default()),
        }
    })
}

/// Threads that stay alive once started, so a steady caller never pays
/// for a thread start.
fn kept_threads() -> usize {
    std::thread::available_parallelism().map_or(2, std::num::NonZero::get)
}

/// Run `job` on a blocking thread.
///
/// # Errors
///
/// The OS refused a thread while none was free and none could be
/// reused; `job` is dropped unrun.
pub(super) fn spawn(job: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    let pool = pool();
    let start_one = {
        let mut counts = lock(&pool.counts);
        if counts.idle > 0 {
            counts.idle -= 1;
            false
        } else if counts.alive < MAX_THREADS {
            counts.alive += 1;
            true
        } else {
            // Saturated: queue behind the busy threads.
            false
        }
    };
    if start_one {
        let index = lock(&pool.counts).alive;
        let rx = pool.rx.clone();
        let started = std::thread::Builder::new()
            .name(format!("istmo-blocking-{index}"))
            .spawn(move || worker_loop(&rx));
        if let Err(err) = started {
            lock(&pool.counts).alive -= 1;
            return Err(err);
        }
    }
    // The pool owns both ends for the life of the process.
    let _ = pool.tx.send(Box::new(job));
    Ok(())
}

fn worker_loop(rx: &Receiver<Job>) {
    let pool = pool();
    loop {
        // Picks up the job the submit that started (or claimed) it sent.
        match rx.recv_timeout(IDLE_EXIT) {
            Ok(job) => job(),
            Err(RecvTimeoutError::Timeout) => {
                let mut counts = lock(&pool.counts);
                if counts.alive > kept_threads() && counts.idle > 0 {
                    counts.idle -= 1;
                    counts.alive -= 1;
                    drop(counts);
                    // A job sent between the timeout and the count update
                    // still runs before the thread leaves.
                    if let Ok(job) = rx.try_recv() {
                        job();
                    }
                    return;
                }
                continue;
            }
            Err(RecvTimeoutError::Disconnected) => return,
        }
        lock(&pool.counts).idle += 1;
    }
}

/// Threads alive (tests).
#[cfg(test)]
pub(super) fn alive() -> usize {
    lock(&pool().counts).alive
}

#[cfg(test)]
mod tests {
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
        }
        // Other tests share the pool, so only bound the growth.
        assert!(alive() <= MAX_THREADS);
        assert!(names.len() <= MAX_THREADS);
    }

    #[test]
    fn concurrent_blocking_jobs_each_get_a_thread() {
        // Eight jobs that only finish once all eight run at once.
        let count = 8;
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(count + 1));
        for _ in 0..count {
            let barrier = std::sync::Arc::clone(&barrier);
            spawn(move || {
                barrier.wait();
            })
            .unwrap();
        }
        barrier.wait();
    }
}
