//! The async workers: a fixed set of persistent threads polling hosted
//! futures that did not finish where they started.
//!
//! A task never holds a worker while it waits: a pending future returns
//! its worker to the pool and is queued again by its waker. So a handful
//! of workers serve any number of waiting calls and streams, and nothing
//! deadlocks on the pool's size — as long as the futures never block the
//! thread polling them (the [`DispatchMode::Inline`] contract).
//!
//! [`DispatchMode::Inline`]: crate::DispatchMode::Inline

use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::task::{Context, Poll, Wake, Waker};

use flume::{Receiver, Sender};

use crate::sync::lock;

/// Most async workers, whatever the core count: their futures only run
/// between two waits, so more threads would mostly wake each other.
const MAX_WORKERS: usize = 4;

type BoxedTask = Pin<Box<dyn Future<Output = ()> + Send + 'static>>;

/// Where a task stands; moves are compare-and-swap on [`Task::state`].
mod state {
    /// Waiting for its waker.
    pub(super) const IDLE: u8 = 0;
    /// In the queue (or about to be).
    pub(super) const SCHEDULED: u8 = 1;
    /// Being polled.
    pub(super) const RUNNING: u8 = 2;
    /// Woken while being polled: poll again once this poll returns.
    pub(super) const NOTIFIED: u8 = 3;
    /// Finished; wakes are ignored.
    pub(super) const DONE: u8 = 4;
}

/// One spawned future and its scheduling state.
struct Task {
    state: AtomicU8,
    // Only the thread that moved the state to RUNNING touches the
    // future, so the lock is never contended; it only makes `Task` Sync.
    future: Mutex<Option<BoxedTask>>,
}

impl Wake for Task {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        let mut current = self.state.load(Ordering::Acquire);
        loop {
            let next = match current {
                state::IDLE => state::SCHEDULED,
                state::RUNNING => state::NOTIFIED,
                // Already queued, already due for another poll, or done.
                _ => return,
            };
            match self
                .state
                .compare_exchange(current, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    if next == state::SCHEDULED {
                        pool().push(Arc::clone(self));
                    }
                    return;
                }
                Err(actual) => current = actual,
            }
        }
    }
}

impl Task {
    /// Poll the future once; the state must be RUNNING. Returns once the
    /// task finished or went back to waiting (it is queued again when it
    /// was woken meanwhile).
    fn run(self: &Arc<Self>) {
        let waker = Waker::from(Arc::clone(self));
        let mut cx = Context::from_waker(&waker);
        let mut slot = lock(&self.future);
        // Hosted futures catch their own panics; this keeps the worker
        // alive for one that does not.
        let polled = slot
            .as_mut()
            .map(|future| catch_unwind(AssertUnwindSafe(|| future.as_mut().poll(&mut cx))));
        drop(slot);
        let finished = match polled {
            Some(Ok(Poll::Pending)) => false,
            Some(Ok(Poll::Ready(()))) | None => true,
            Some(Err(_)) => {
                tracing::error!("an istmo task panicked; dropped");
                true
            }
        };
        if finished {
            // Drop the future (and whatever it holds) before reporting done.
            *lock(&self.future) = None;
            self.state.store(state::DONE, Ordering::Release);
            return;
        }
        if self
            .state
            .compare_exchange(
                state::RUNNING,
                state::IDLE,
                Ordering::AcqRel,
                Ordering::Acquire,
            )
            .is_err()
        {
            // NOTIFIED: woken during the poll. Queue it rather than poll
            // again here, so one busy task cannot starve the others.
            self.state.store(state::SCHEDULED, Ordering::Release);
            pool().push(Arc::clone(self));
        }
    }
}

/// The persistent async workers, started on first use.
struct Pool {
    tx: Sender<Arc<Task>>,
    rx: Receiver<Arc<Task>>,
    workers: AtomicUsize,
    /// Serialises worker starts so the count stays exact.
    starting: Mutex<()>,
}

fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let (tx, rx) = flume::unbounded();
        Pool {
            tx,
            rx,
            workers: AtomicUsize::new(0),
            starting: Mutex::new(()),
        }
    })
}

/// How many async workers the pool runs on this machine.
fn worker_target() -> usize {
    std::thread::available_parallelism()
        .map_or(2, std::num::NonZero::get)
        .clamp(2, MAX_WORKERS)
}

impl Pool {
    fn push(&self, task: Arc<Task>) {
        self.ensure_workers();
        // The pool owns both ends for the life of the process.
        let _ = self.tx.send(task);
    }

    /// Start the workers that are missing. Cheap once they all run.
    fn ensure_workers(&self) {
        let target = worker_target();
        if self.workers.load(Ordering::Acquire) >= target {
            return;
        }
        let _starting = lock(&self.starting);
        while self.workers.load(Ordering::Acquire) < target {
            let index = self.workers.load(Ordering::Acquire);
            let rx = self.rx.clone();
            let started = std::thread::Builder::new()
                .name(format!("istmo-worker-{index}"))
                .spawn(move || worker_loop(&rx));
            match started {
                Ok(_) => {
                    self.workers.fetch_add(1, Ordering::AcqRel);
                }
                Err(err) => {
                    // Queued tasks wait for the next push to retry.
                    tracing::error!(%err, index, "could not start an istmo worker");
                    return;
                }
            }
        }
    }
}

fn worker_loop(rx: &Receiver<Arc<Task>>) {
    while let Ok(task) = rx.recv() {
        task.state.store(state::RUNNING, Ordering::Release);
        task.run();
    }
}

/// Run `future` on the calling thread until it first waits; the async
/// workers poll it from then on. Returns `true` when it finished here.
pub(super) fn run_here(future: impl Future<Output = ()> + Send + 'static) -> bool {
    let task = Arc::new(Task {
        state: AtomicU8::new(state::RUNNING),
        future: Mutex::new(Some(Box::pin(future))),
    });
    task.run();
    task.state.load(Ordering::Acquire) == state::DONE
}

/// Queue `future` on the async workers.
pub(super) fn spawn(future: impl Future<Output = ()> + Send + 'static) {
    let task = Arc::new(Task {
        state: AtomicU8::new(state::SCHEDULED),
        future: Mutex::new(Some(Box::pin(future))),
    });
    pool().push(task);
}

/// Number of async workers started so far (tests).
#[cfg(test)]
pub(super) fn started_workers() -> usize {
    pool().workers.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn ready_futures_finish_on_the_caller() {
        let caller = std::thread::current().id();
        let (tx, rx) = flume::bounded(1);
        assert!(run_here(async move {
            tx.send(std::thread::current().id()).unwrap();
        }));
        assert_eq!(rx.recv().unwrap(), caller);
    }

    #[test]
    fn pending_futures_move_to_a_worker() {
        let (gate_tx, gate_rx) = flume::bounded::<()>(1);
        let (done_tx, done_rx) = flume::bounded(1);
        let caller = std::thread::current().id();
        assert!(!run_here(async move {
            gate_rx.recv_async().await.unwrap();
            done_tx.send(std::thread::current().id()).unwrap();
        }));
        gate_tx.send(()).unwrap();
        let finished_on = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_ne!(finished_on, caller);
    }

    #[test]
    fn many_waiting_tasks_share_few_workers() {
        let count = 256;
        let (gate_tx, gate_rx) = flume::unbounded::<()>();
        let (done_tx, done_rx) = flume::unbounded();
        for _ in 0..count {
            let gate = gate_rx.clone();
            let done = done_tx.clone();
            spawn(async move {
                gate.recv_async().await.unwrap();
                done.send(()).unwrap();
            });
        }
        for _ in 0..count {
            gate_tx.send(()).unwrap();
        }
        for _ in 0..count {
            done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(started_workers() <= MAX_WORKERS);
    }

    #[test]
    fn a_wake_during_the_poll_polls_again() {
        // The future wakes itself before returning Pending: it must be
        // polled again even though nobody else wakes it.
        let (done_tx, done_rx) = flume::bounded(1);
        let mut polls = 0;
        let future = std::future::poll_fn(move |cx| {
            polls += 1;
            if polls < 3 {
                cx.waker().wake_by_ref();
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        run_here(async move {
            future.await;
            done_tx.send(()).unwrap();
        });
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }

    #[test]
    fn a_panicking_task_leaves_the_workers_alive() {
        spawn(async { panic!("task bug") });
        let (tx, rx) = flume::bounded(1);
        for _ in 0..MAX_WORKERS * 2 {
            let tx = tx.clone();
            spawn(async move {
                let _ = tx.try_send(());
            });
        }
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
