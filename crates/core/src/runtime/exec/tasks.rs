//! The async workers: a fixed set of persistent threads polling hosted
//! futures that did not finish where they started.
//!
//! A task never holds a worker while it waits: a pending future returns
//! its worker to the pool and is queued again by its waker. So a handful
//! of workers serve any number of waiting calls and streams, and nothing
//! deadlocks on the pool's size — as long as the futures never block the
//! thread polling it (the [`DispatchMode::Inline`] contract).
//!
//! [`DispatchMode::Inline`]: crate::DispatchMode::Inline

use std::cell::Cell;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering, fence};
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

/// Tasks a worker runs from its own slot in a row before it takes the
/// shared queue again.
const MAX_LOCAL_RUN: u32 = 32;

mod worker {
    // The initialiser is a `const` block already; clippy's
    // `missing_const_for_thread_local` still fires on some targets
    // (aarch64-linux-android with 1.100).
    #![allow(
        clippy::missing_const_for_thread_local,
        reason = "false positive on some targets; the initialiser is const"
    )]

    use std::cell::Cell;

    thread_local! {
        /// On an async worker thread: its index in [`super::Pool::slots`].
        pub(super) static INDEX: Cell<Option<usize>> = const { Cell::new(None) };
    }
}

use worker::INDEX;

/// Queue a woken task: in the current worker's own slot when a worker
/// woke it, on the shared queue otherwise.
///
/// The slot saves waking another worker for a task woken by the one it
/// is about to follow (a stream encoder waking its pump), but the waker
/// may keep polling for long after the wake. So the slot is stealable:
/// a spinning worker takes it, and when none spins and one is parked,
/// that one is woken to take it unless this worker gets there first.
fn schedule(task: Arc<Task>) {
    let pool = pool();
    let Some(index) = INDEX.with(Cell::get) else {
        pool.push(task);
        return;
    };
    let displaced = lock(&pool.slots[index]).replace(task);
    if let Some(displaced) = displaced {
        pool.push(displaced);
    }
    pool.offer_slots();
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
                        schedule(Arc::clone(self));
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
        let finished = match polled {
            Some(Ok(Poll::Pending)) => false,
            Some(Ok(Poll::Ready(()))) | None => true,
            Some(Err(_)) => {
                tracing::error!("an istmo task panicked; dropped");
                true
            }
        };
        if finished {
            // Drop the future (and whatever it holds) before reporting
            // done; a panicking drop must not take the worker with it.
            let future = slot.take();
            drop(slot);
            if catch_unwind(AssertUnwindSafe(move || drop(future))).is_err() {
                tracing::error!("dropping a finished istmo task panicked");
            }
            self.state.store(state::DONE, Ordering::Release);
            return;
        }
        drop(slot);
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

/// What the shared queue carries.
enum Job {
    Run(Arc<Task>),
    /// Wakes a parked worker to look at the other workers' slots.
    Steal,
}

/// The persistent async workers, started on first use.
struct Pool {
    tx: Sender<Job>,
    rx: Receiver<Job>,
    workers: AtomicUsize,
    /// Serialises worker starts so the count stays exact.
    starting: Mutex<()>,
    /// Each worker's own slot: the task it runs next.
    slots: [Mutex<Option<Arc<Task>>>; MAX_WORKERS],
    /// Workers polling the queue and the slots before they park.
    searching: AtomicUsize,
    /// Workers parked on the shared queue.
    parked: AtomicUsize,
    /// A [`Job::Steal`] is on its way; another adds nothing.
    steal_sent: AtomicBool,
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
            slots: std::array::from_fn(|_| Mutex::new(None)),
            searching: AtomicUsize::new(0),
            parked: AtomicUsize::new(0),
            steal_sent: AtomicBool::new(false),
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
        let _ = self.tx.send(Job::Run(task));
    }

    /// A slot was just filled: make sure an idle worker can take it.
    ///
    /// Pairs with [`park`](Self::park): the filler writes the slot, then
    /// reads the counters; a parking worker writes the counters, then
    /// reads the slots, with a sequentially consistent fence between on
    /// both sides. So either the parking worker sees the slot, or this
    /// sees it parked and wakes it.
    fn offer_slots(&self) {
        fence(Ordering::SeqCst);
        if self.searching.load(Ordering::SeqCst) == 0
            && self.parked.load(Ordering::SeqCst) > 0
            && !self.steal_sent.swap(true, Ordering::AcqRel)
        {
            let _ = self.tx.send(Job::Steal);
        }
    }

    /// Take a task from any worker's slot.
    fn steal(&self) -> Option<Arc<Task>> {
        let started = self.workers.load(Ordering::Acquire).min(MAX_WORKERS);
        self.slots[..started]
            .iter()
            .find_map(|slot| lock(slot).take())
    }

    /// The next task from the shared queue or a slot, without parking.
    fn poll_once(&self) -> Option<Arc<Task>> {
        match self.rx.try_recv() {
            Ok(Job::Run(task)) => Some(task),
            Ok(Job::Steal) => {
                self.steal_sent.store(false, Ordering::Release);
                self.steal()
            }
            Err(_) => self.steal(),
        }
    }

    /// The next task for an idle worker: spin briefly, then park. `None`
    /// once the queue is gone.
    fn next(&self) -> Option<Arc<Task>> {
        static WAIT: crate::spin::SpinWait = crate::spin::SpinWait::new();
        self.searching.fetch_add(1, Ordering::SeqCst);
        // A task woken right after the last one finished is picked up
        // without parking the worker.
        if let Some(task) = WAIT.spin(|| self.poll_once()) {
            self.searching.fetch_sub(1, Ordering::SeqCst);
            return Some(task);
        }
        self.park()
    }

    fn park(&self) -> Option<Arc<Task>> {
        self.parked.fetch_add(1, Ordering::SeqCst);
        self.searching.fetch_sub(1, Ordering::SeqCst);
        fence(Ordering::SeqCst);
        let found = loop {
            if let Some(task) = self.poll_once() {
                break Some(task);
            }
            match self.rx.recv() {
                Ok(Job::Run(task)) => break Some(task),
                Ok(Job::Steal) => {
                    self.steal_sent.store(false, Ordering::Release);
                    if let Some(task) = self.steal() {
                        break Some(task);
                    }
                }
                Err(_) => break None,
            }
        };
        self.parked.fetch_sub(1, Ordering::SeqCst);
        found
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
            let started = std::thread::Builder::new()
                .name(format!("istmo-worker-{index}"))
                .spawn(move || worker_loop(index));
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

fn worker_loop(index: usize) {
    let pool = pool();
    INDEX.with(|slot| slot.set(Some(index)));
    let mut local_run = 0u32;
    loop {
        let local = lock(&pool.slots[index]).take();
        let task = match local {
            Some(task) if local_run < MAX_LOCAL_RUN => {
                local_run += 1;
                task
            }
            Some(task) => {
                // Let the shared queue in: the local task waits its turn.
                local_run = 0;
                pool.push(task);
                continue;
            }
            None => {
                local_run = 0;
                let Some(task) = pool.next() else { return };
                task
            }
        };
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

/// The current thread is an async worker.
pub(super) fn on_worker() -> bool {
    INDEX.with(Cell::get).is_some()
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
    fn a_task_woken_by_a_worker_can_run_next_on_that_worker() {
        // Another worker may steal the woken task; over a few rounds the
        // waker still runs it itself at least once.
        let mut same = 0;
        for _ in 0..20 {
            let (item_tx, item_rx) = flume::bounded::<()>(1);
            let (seen_tx, seen_rx) = flume::bounded(2);
            let consumer_seen = seen_tx.clone();
            // The consumer waits; the producer wakes it from a worker.
            spawn(async move {
                item_rx.recv_async().await.unwrap();
                consumer_seen.send(std::thread::current().id()).unwrap();
            });
            std::thread::sleep(Duration::from_millis(5));
            spawn(async move {
                seen_tx.send(std::thread::current().id()).unwrap();
                item_tx.send_async(()).await.unwrap();
            });
            let producer = seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            let consumer = seen_rx.recv_timeout(Duration::from_secs(5)).unwrap();
            same += usize::from(producer == consumer);
        }
        assert!(same > 0, "the waking worker never ran the woken task");
    }

    #[test]
    fn a_woken_task_does_not_wait_out_its_wakers_long_poll() {
        let (item_tx, item_rx) = flume::bounded::<()>(1);
        let (done_tx, done_rx) = flume::bounded(1);
        spawn(async move {
            item_rx.recv_async().await.unwrap();
            done_tx.send(std::time::Instant::now()).unwrap();
        });
        std::thread::sleep(Duration::from_millis(5));
        let (woke_tx, woke_rx) = flume::bounded(1);
        spawn(async move {
            item_tx.try_send(()).unwrap();
            woke_tx.send(std::time::Instant::now()).unwrap();
            // Keeps polling long after the wake.
            std::thread::sleep(Duration::from_millis(500));
        });
        let woke = woke_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        let ran = done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            ran.duration_since(woke) < Duration::from_millis(250),
            "the woken task waited {:?} behind its waker",
            ran.duration_since(woke)
        );
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

    struct PanicOnDrop;

    impl Drop for PanicOnDrop {
        fn drop(&mut self) {
            panic!("drop bug");
        }
    }

    #[test]
    fn a_task_whose_drop_panics_leaves_the_workers_alive() {
        for _ in 0..MAX_WORKERS * 2 {
            let guard = PanicOnDrop;
            spawn(async move {
                let _guard = guard;
            });
        }
        let (tx, rx) = flume::bounded(1);
        spawn(async move {
            let _ = tx.try_send(());
        });
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
}
