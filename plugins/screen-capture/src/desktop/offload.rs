//! Runs blocking backend work on the plugin's own threads, so every
//! host method stays a future that completes on any executor (hosts
//! run inline on the caller unless `#[istmo::blocking]`).
//!
//! The threads are a small shared pool: at most [`MAX_WORKERS`] run at
//! once, idle ones exit after [`IDLE_EXIT`], and at most [`QUEUE`] jobs
//! wait. A flood of calls (methods open to every origin included) is
//! refused instead of costing one OS thread per call.

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

impl Workers {
    /// Queue `job`; it comes back when the queue is full.
    fn submit(&'static self, job: Job) -> Result<(), Job> {
        if self.idle.load(Ordering::Acquire) == 0 {
            self.grow();
        }
        self.jobs.try_send(job).map_err(|err| match err {
            flume::TrySendError::Full(job) | flume::TrySendError::Disconnected(job) => job,
        })
    }

    fn grow(&'static self) {
        let reserved = self
            .live
            .fetch_update(Ordering::AcqRel, Ordering::Acquire, |live| {
                (live < MAX_WORKERS).then_some(live + 1)
            })
            .is_ok();
        if !reserved {
            return;
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
        if !self.queue.is_empty() && self.idle.load(Ordering::Acquire) == 0 {
            self.grow();
        }
    }
}

/// Whether the caller of an offloaded call is still waiting for it.
#[derive(Debug, Clone, Default)]
pub(crate) struct Caller {
    gone: Arc<AtomicBool>,
}

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

fn internal(step: &str, message: &str) -> CaptureError {
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
