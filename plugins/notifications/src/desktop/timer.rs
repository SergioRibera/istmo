//! The host's one timer thread: scheduled deliveries, queued live
//! updates and timeouts all wait on it.

use std::sync::{Arc, OnceLock, Weak};
use std::time::Duration;

use crate::schedule::Wakeup;

/// Longest single wait: the thread re-reads the wall clock at least this
/// often, so a suspend or a clock change delays a deadline by at most
/// this much.
const MAX_WAIT: Duration = Duration::from_secs(30);

/// What the timer thread calls at a deadline; returns the next one.
pub(crate) trait Tick: Send + Sync + 'static {
    fn tick(self: &Arc<Self>) -> Option<i64>;
    fn now_ms(&self) -> i64;
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
