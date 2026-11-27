//! The host's two threads: the timer (scheduled deliveries, queued live
//! updates and timeouts all wait on it) and the report worker (what a
//! backend reports through its sink is handled there, never on the
//! backend's own thread).

use std::cell::Cell;
use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crate::schedule::Wakeup;

/// Longest single wait: the thread re-reads the wall clock at least this
/// often, so a suspend or a clock change delays a deadline by at most
/// this much.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// The deadline moved (to `None`: nothing left to wait for).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Moved(pub(crate) Option<i64>);

/// What the timer thread calls at a deadline; returns the next one.
pub(crate) trait Tick: Send + Sync + 'static {
    fn tick(self: &Arc<Self>) -> Option<i64>;
    fn now_ms(&self) -> i64;
    /// Called at every wake-up before the deadline (at least every
    /// [`MAX_WAIT`]): `Some(deadline)` when the deadline moved (the
    /// device's zone changed).
    fn idle(self: &Arc<Self>) -> Option<Moved>;
}

/// One pending deadline, served by a thread started on the first arm.
pub(crate) struct Timer<T: Tick> {
    target: Weak<T>,
    tx: OnceLock<flume::Sender<Option<i64>>>,
}

impl<T: Tick> std::fmt::Debug for Timer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Timer")
            .field("started", &self.tx.get().is_some())
            .finish_non_exhaustive()
    }
}

impl<T: Tick> Timer<T> {
    pub(crate) const fn new(target: Weak<T>) -> Self {
        Self {
            target,
            tx: OnceLock::new(),
        }
    }

    fn sender(&self) -> &flume::Sender<Option<i64>> {
        self.tx.get_or_init(|| {
            let (tx, rx) = flume::unbounded();
            let target = self.target.clone();
            let spawned = std::thread::Builder::new()
                .name("istmo-notifications-timer".to_owned())
                .spawn(move || run(&target, &rx));
            if let Err(err) = spawned {
                tracing::warn!(%err, "notifications: the timer thread could not start");
            }
            tx
        })
    }
}

impl<T: Tick> Wakeup for Timer<T> {
    fn arm(&self, deadline_ms: Option<i64>) {
        if deadline_ms.is_none() && self.tx.get().is_none() {
            return;
        }
        let _sent = self.sender().send(deadline_ms);
    }
}

fn run<T: Tick>(target: &Weak<T>, rx: &flume::Receiver<Option<i64>>) {
    let mut deadline: Option<i64> = None;
    loop {
        let wait = match deadline {
            None => None,
            Some(at) => {
                let Some(host) = target.upgrade() else {
                    return;
                };
                let left = at.saturating_sub(host.now_ms());
                drop(host);
                Some(Duration::from_millis(u64::try_from(left).unwrap_or(0)).min(MAX_WAIT))
            }
        };
        let message = match wait {
            None => rx.recv().map_err(|_| flume::RecvTimeoutError::Disconnected),
            Some(wait) if wait.is_zero() => Err(flume::RecvTimeoutError::Timeout),
            Some(wait) => rx.recv_timeout(wait),
        };
        match message {
            Ok(next) => deadline = next,
            Err(flume::RecvTimeoutError::Disconnected) => return,
            Err(flume::RecvTimeoutError::Timeout) => {
                let Some(host) = target.upgrade() else {
                    return;
                };
                if deadline.is_some_and(|at| at <= host.now_ms()) {
                    deadline = host.tick();
                } else if let Some(Moved(moved)) = host.idle() {
                    deadline = moved;
                }
            }
        }
    }
}

/// A timer that never runs on its own: tests call `tick` themselves.
#[derive(Debug, Default)]
pub(crate) struct ManualTimer {
    pub(crate) armed: std::sync::Mutex<Option<i64>>,
}

impl Wakeup for ManualTimer {
    fn arm(&self, deadline_ms: Option<i64>) {
        *istmo_core::sync::lock(&self.armed) = deadline_ms;
    }
}

/// Either timer, chosen at construction.
pub(crate) enum HostTimer<T: Tick> {
    Thread(Timer<T>),
    Manual(Arc<ManualTimer>),
}

impl<T: Tick> std::fmt::Debug for HostTimer<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Thread(timer) => timer.fmt(f),
            Self::Manual(timer) => timer.fmt(f),
        }
    }
}

impl<T: Tick> Wakeup for HostTimer<T> {
    fn arm(&self, deadline_ms: Option<i64>) {
        match self {
            Self::Thread(timer) => timer.arm(deadline_ms),
            Self::Manual(timer) => timer.arm(deadline_ms),
        }
    }
}

/// What the report worker calls for each queued message.
pub(crate) trait Serve<M>: Send + Sync + 'static {
    fn serve(self: &Arc<Self>, message: M);
}

thread_local! {
    /// Set on the report worker's thread.
    static ON_WORKER: Cell<bool> = const { Cell::new(false) };
}

/// Whether the current thread is a report worker.
pub(crate) fn on_worker() -> bool {
    ON_WORKER.with(Cell::get)
}

/// One thread, started on the first message, that serves messages in
/// order. A sink call only queues: the backend's thread never runs host
/// code (which may call the backend back) and a sink call made while the
/// backend starts never re-enters the host.
pub(crate) struct Worker<M: Send + 'static> {
    /// `None` when the thread could not start: messages are then served
    /// on the caller's thread.
    tx: OnceLock<Option<flume::Sender<M>>>,
}

impl<M: Send + 'static> std::fmt::Debug for Worker<M> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Worker")
            .field("started", &self.tx.get().is_some())
            .finish()
    }
}

impl<M: Send + 'static> Default for Worker<M> {
    fn default() -> Self {
        Self {
            tx: OnceLock::new(),
        }
    }
}

impl<M: Send + 'static> Worker<M> {
    /// Queues `message` for `target`.
    pub(crate) fn send<T: Serve<M>>(&self, target: &Arc<T>, message: M) {
        let tx = self.tx.get_or_init(|| {
            let (tx, rx) = flume::unbounded();
            let weak = Arc::downgrade(target);
            let spawned = std::thread::Builder::new()
                .name("istmo-notifications-reports".to_owned())
                .spawn(move || serve_all(&weak, &rx));
            match spawned {
                Ok(_) => Some(tx),
                Err(err) => {
                    tracing::warn!(%err, "notifications: the report worker could not start; reports run inline");
                    None
                }
            }
        });
        let unsent = match tx {
            Some(tx) => tx.send(message).err().map(flume::SendError::into_inner),
            None => Some(message),
        };
        if let Some(message) = unsent {
            target.serve(message);
        }
    }
}

fn serve_all<T: Serve<M>, M: Send + 'static>(target: &Weak<T>, rx: &flume::Receiver<M>) {
    ON_WORKER.with(|flag| flag.set(true));
    while let Ok(message) = rx.recv() {
        let Some(host) = target.upgrade() else {
            return;
        };
        let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| host.serve(message)));
        if served.is_err() {
            tracing::error!("notifications: a backend report panicked; the worker goes on");
        }
    }
}
