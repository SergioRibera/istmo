//! Runs blocking backend work on the plugin's own threads, so every
//! host method stays a future that completes on any executor (hosts
//! run inline on the caller unless `#[istmo::blocking]`).
//!
//! The threads are a small shared pool: at most [`MAX_WORKERS`] run at
//! once, idle ones exit after [`IDLE_EXIT`], and at most [`QUEUE`] jobs
//! wait. A flood of calls (methods open to every origin included) is
//! refused instead of costing one OS thread per call. Teardown never
//! goes through the pool: it runs on a thread of its own, so it never
//! waits behind host calls (a consent dialog can hold a worker for
//! minutes).

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use crate::types::{BackendDetail, CaptureError};

/// Threads the pool runs at most.
const MAX_WORKERS: usize = 8;
/// Jobs waiting for a thread before new ones are refused.
const QUEUE: usize = 64;
/// How long an idle worker waits for a job before it exits.
const IDLE_EXIT: Duration = Duration::from_secs(30);
/// Calls open to every origin (no `HostCtx`, so nothing to admit) that
/// may be in the pool at once: a remote flood of them keeps the other
/// workers and most of the queue free for the app's own calls.
const OPEN_CALLS: usize = MAX_WORKERS / 2;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Workers {
    jobs: flume::Sender<Job>,
    queue: flume::Receiver<Job>,
    live: AtomicUsize,
    idle: AtomicUsize,
}

fn workers() -> &'static Workers {
    static WORKERS: OnceLock<Workers> = OnceLock::new();
    WORKERS.get_or_init(|| {
        let (jobs, queue) = flume::bounded(QUEUE);
        Workers {
            jobs,
            queue,
            live: AtomicUsize::new(0),
            idle: AtomicUsize::new(0),
        }
    })
}

/// Whether a job about to be queued needs a new worker: every idle one
/// may already be promised to a job still in the queue (an idle worker
/// woken for job 1 must not leave job 2 queued behind it).
const fn needs_worker(queued: usize, idle: usize) -> bool {
    queued >= idle
}

impl Workers {
    /// Queue `job`; it comes back when the queue is full.
    fn submit(&'static self, job: Job) -> Result<(), Job> {
        if needs_worker(self.queue.len(), self.idle.load(Ordering::Acquire)) {
            self.grow();
        }
        self.jobs.try_send(job).map_err(|err| match err {
            flume::TrySendError::Full(job) | flume::TrySendError::Disconnected(job) => job,
        })
    }

    fn grow(&'static self) {
        // Reserve a thread slot (a compare-exchange loop: `fetch_update` is
        // deprecated on newer toolchains, `try_update` too new for the MSRV).
        let mut live = self.live.load(Ordering::Acquire);
        loop {
            if live >= MAX_WORKERS {
                return;
            }
            match self.live.compare_exchange_weak(
                live,
                live + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => break,
                Err(now) => live = now,
            }
        }
        let spawned = std::thread::Builder::new()
            .name("istmo-screen-capture-worker".to_owned())
            .spawn(move || self.run());
        if let Err(err) = spawned {
            self.live.fetch_sub(1, Ordering::AcqRel);
            tracing::warn!(%err, "screen-capture: could not start a worker thread");
        }
    }

    fn run(&'static self) {
        loop {
            self.idle.fetch_add(1, Ordering::AcqRel);
            let job = self.queue.recv_timeout(IDLE_EXIT);
            self.idle.fetch_sub(1, Ordering::AcqRel);
            let Ok(job) = job else { break };
            // A panicking job loses only its own result: its sender is
            // dropped and the caller sees an error.
            let _outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        }
        self.live.fetch_sub(1, Ordering::AcqRel);
        // A job queued while this worker was deciding to exit still
        // needs a thread.
        if !self.queue.is_empty()
            && needs_worker(self.queue.len(), self.idle.load(Ordering::Acquire))
        {
            self.grow();
        }
    }
}

/// Run `job` on a thread of its own without waiting for it, for
/// teardown that must never block the thread that triggered it (a handle
/// dropped in async code), nor queue behind host calls in the pool.
/// When no thread starts, the job is dropped here, and with it whatever
/// it owns (a teardown job is a drop of the session it carries).
pub(crate) fn detach(what: &'static str, job: impl FnOnce() + Send + 'static) {
    let spawned = std::thread::Builder::new()
        .name(format!("istmo-screen-capture-{what}"))
        .spawn(job);
    if let Err(err) = spawned {
        tracing::error!(%err, what, "screen-capture: teardown thread could not start");
    }
}

/// A cap on concurrent calls of one kind.
#[derive(Debug)]
struct Gate {
    in_flight: AtomicUsize,
    limit: usize,
}

/// One admitted call; frees its place on drop.
struct Pass(&'static Gate);

impl Drop for Pass {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Gate {
    const fn new(limit: usize) -> Self {
        Self {
            in_flight: AtomicUsize::new(0),
            limit,
        }
    }

    /// A place, or `None` when `limit` calls are in flight.
    fn enter(&'static self) -> Option<Pass> {
        let mut now = self.in_flight.load(Ordering::Acquire);
        loop {
            if now >= self.limit {
                return None;
            }
            match self.in_flight.compare_exchange_weak(
                now,
                now + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(Pass(self)),
                Err(current) => now = current,
            }
        }
    }
}

/// [`offload`] for methods open to every origin: at most [`OPEN_CALLS`]
/// of them run or wait at once, the rest are refused at once. The place
/// is held by the job itself, so a caller that drops its future does not
/// free it while the work still runs.
///
/// # Errors
/// As [`offload`], and `Protocol` when the cap is reached.
pub(crate) async fn offload_open<T, F>(what: &'static str, work: F) -> Result<T, CaptureError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
{
    static OPEN: Gate = Gate::new(OPEN_CALLS);
    offload_gated(&OPEN, what, work).await
}

async fn offload_gated<T, F>(
    gate: &'static Gate,
    what: &'static str,
    work: F,
) -> Result<T, CaptureError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
{
    let pass = gate
        .enter()
        .ok_or_else(|| internal(what, "too many calls open to every origin in flight"))?;
    offload(what, move || {
        let _pass = pass;
        work()
    })
    .await
}

/// Whether the caller of an offloaded call is still waiting for it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Caller {
    gone: Arc<AtomicBool>,
}

/// Two handles are equal when they watch the same call.
impl PartialEq for Caller {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.gone, &other.gone)
    }
}

impl Eq for Caller {}

impl Caller {
    /// A caller that already gave up.
    #[cfg(test)]
    pub(crate) fn abandoned() -> Self {
        Self {
            gone: Arc::new(AtomicBool::new(true)),
        }
    }

    /// `true` once the caller dropped the call's future: work that would
    /// start something only that caller can stop (a session) must not.
    pub(crate) fn is_gone(&self) -> bool {
        self.gone.load(Ordering::Acquire)
    }
}

/// Marks the caller gone when the awaiting future is dropped.
struct Waiting(Arc<AtomicBool>);

impl Drop for Waiting {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// Run `work` on a plugin thread and await its result.
///
/// # Errors
/// The work's own error, `Protocol` when the pool is saturated or the
/// work panicked.
pub(crate) async fn offload<T, F>(what: &'static str, work: F) -> Result<T, CaptureError>
where
    T: Send + 'static,
    F: FnOnce() -> Result<T, CaptureError> + Send + 'static,
{
    offload_owned(what, move |_| work(), drop).await
}

/// [`offload`] for work whose success the caller must own: when the
/// caller stopped waiting, `undo` receives the result instead (and
/// `work` can check [`Caller::is_gone`] before committing).
///
/// # Errors
/// As [`offload`].
pub(crate) async fn offload_owned<T, F, U>(
    what: &'static str,
    work: F,
    undo: U,
) -> Result<T, CaptureError>
where
    T: Send + 'static,
    F: FnOnce(&Caller) -> Result<T, CaptureError> + Send + 'static,
    U: FnOnce(T) + Send + 'static,
{
    let caller = Caller::default();
    let waiting = Waiting(Arc::clone(&caller.gone));
    let (tx, rx) = oneshot::channel();
    let job: Job = Box::new(move || {
        let result = work(&caller);
        if let Err(unsent) = tx.send(result)
            && let Ok(unwanted) = unsent.into_inner()
        {
            undo(unwanted);
        }
    });
    workers()
        .submit(job)
        .map_err(|_| internal(what, "too many screen-capture calls in flight"))?;
    let result = rx
        .await
        .map_err(|_| internal(what, "the worker panicked"))?;
    drop(waiting);
    result
}

/// A failure of the plugin itself (no backend involved), naming `step`.
pub(crate) fn internal(step: &str, message: &str) -> CaptureError {
    CaptureError::Protocol(BackendDetail {
        backend: None,
        step: step.to_owned(),
        message: message.to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn calls_reuse_a_bounded_set_of_threads() {
        for i in 0..200_u32 {
            let got = pollster::block_on(offload("test", move || Ok(i))).expect("offload");
            assert_eq!(got, i);
        }
        assert!(workers().live.load(Ordering::Acquire) <= MAX_WORKERS);
    }

    #[test]
    fn a_job_queued_behind_promised_workers_gets_a_new_one() {
        assert!(needs_worker(0, 0));
        assert!(
            needs_worker(1, 1),
            "the idle worker is promised to the queued job"
        );
        assert!(needs_worker(3, 2));
        assert!(!needs_worker(0, 1));
        assert!(!needs_worker(1, 2));
    }

    #[test]
    fn a_quick_call_does_not_wait_behind_a_long_one() {
        let (release_tx, release_rx) = flume::bounded::<()>(1);
        let long = std::thread::spawn(move || {
            pollster::block_on(offload("long", move || {
                let _released = release_rx.recv_timeout(Duration::from_secs(10));
                Ok(())
            }))
        });
        let start = Instant::now();
        for i in 0..4_u32 {
            let got = pollster::block_on(offload("quick", move || Ok(i))).expect("quick");
            assert_eq!(got, i);
        }
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "quick calls ran beside the long one"
        );
        release_tx.send(()).expect("release");
        long.join().expect("long").expect("long call");
    }

    #[test]
    fn calls_open_to_every_origin_are_capped() {
        static GATE: Gate = Gate::new(2);
        let (release_tx, release_rx) = flume::unbounded::<()>();
        let (started_tx, started_rx) = flume::unbounded::<()>();
        let busy: Vec<_> = (0..2)
            .map(|_| {
                let (release_rx, started_tx) = (release_rx.clone(), started_tx.clone());
                std::thread::spawn(move || {
                    pollster::block_on(offload_gated(&GATE, "backends", move || {
                        let _started = started_tx.send(());
                        let _released = release_rx.recv_timeout(Duration::from_secs(10));
                        Ok(())
                    }))
                })
            })
            .collect();
        for _ in 0..2 {
            started_rx
                .recv_timeout(Duration::from_secs(5))
                .expect("started");
        }
        let refused = pollster::block_on(offload_gated(&GATE, "backends", || Ok(())));
        assert!(
            matches!(refused, Err(CaptureError::Protocol(_))),
            "{refused:?}"
        );
        for _ in 0..2 {
            release_tx.send(()).expect("release");
        }
        for call in busy {
            call.join().expect("call").expect("ran");
        }
        assert!(
            pollster::block_on(offload_gated(&GATE, "backends", || Ok(()))).is_ok(),
            "places come back when the work ends"
        );
    }

    #[test]
    fn teardown_runs_on_its_own_thread() {
        let (tx, rx) = flume::bounded(1);
        detach("teardown", move || {
            let name = std::thread::current().name().map(str::to_owned);
            let _sent = tx.send(name);
        });
        let name = rx.recv_timeout(Duration::from_secs(5)).expect("ran");
        assert_eq!(name.as_deref(), Some("istmo-screen-capture-teardown"));
    }

    #[test]
    fn an_abandoned_call_hands_its_result_to_undo() {
        let undone = Arc::new(AtomicUsize::new(0));
        let saw_gone = Arc::new(AtomicBool::new(false));
        let (started_tx, started_rx) = flume::bounded::<()>(1);
        let (go_tx, go_rx) = flume::bounded::<()>(1);
        let work = {
            let saw_gone = Arc::clone(&saw_gone);
            move |caller: &Caller| {
                let _started = started_tx.send(());
                let _go = go_rx.recv();
                saw_gone.store(caller.is_gone(), Ordering::SeqCst);
                Ok(7_u32)
            }
        };
        let undo = {
            let undone = Arc::clone(&undone);
            move |value: u32| {
                assert_eq!(value, 7);
                undone.fetch_add(1, Ordering::SeqCst);
            }
        };
        let mut future = Box::pin(offload_owned("test", work, undo));
        let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(future.as_mut().poll(&mut cx).is_pending());
        started_rx.recv().expect("started");
        drop(future);
        go_tx.send(()).expect("go");
        let deadline = Instant::now() + Duration::from_secs(5);
        while undone.load(Ordering::SeqCst) == 0 {
            assert!(Instant::now() < deadline, "undo never ran");
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(saw_gone.load(Ordering::SeqCst));
    }
}
