//! Main-thread dispatcher abstraction.
//!
//! On Android this is backed by a `Handler` bound to the main `Looper`; on
//! iOS by `dispatch_async` on the main queue. Neither backend exists at M0 —
//! this module provides the trait plus two implementations used by tests and
//! by desktop / mock backends:
//!
//! * [`InlineMainThread`] runs the closure immediately on the caller's thread.
//! * [`MockMainThread`] queues closures for later inspection via [`drain`].
//!
//! [`drain`]: MockMainThread::drain

use std::fmt;

use flume::{Receiver, Sender, bounded};

use crate::sync::lock;

/// Any type able to schedule work on the platform main thread.
pub trait MainThread: Send + Sync + fmt::Debug {
    /// Enqueue a closure to be executed on the main thread.
    fn run(&self, task: Task);
}

/// Boxed closure enqueued on a [`MainThread`] dispatcher.
pub type Task = Box<dyn FnOnce() + Send + 'static>;

/// Dispatcher that runs every task inline on the caller's thread. Useful for
/// desktop / test scenarios where "main thread" has no special meaning.
#[derive(Debug, Default)]
pub struct InlineMainThread;

impl MainThread for InlineMainThread {
    fn run(&self, task: Task) {
        task();
    }
}

/// Dispatcher that captures tasks in a bounded queue for deterministic tests.
///
/// Tasks are executed only when [`drain`] is called; the returned count is the
/// number of tasks that ran on that call.
///
/// [`drain`]: MockMainThread::drain
pub struct MockMainThread {
    tx: Sender<Task>,
    rx: std::sync::Mutex<Receiver<Task>>,
}

impl Default for MockMainThread {
    fn default() -> Self {
        Self::with_capacity(256)
    }
}

impl MockMainThread {
    /// Creates a dispatcher with the default queue capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a dispatcher with an explicit queue capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, rx) = bounded(capacity);
        Self {
            tx,
            rx: std::sync::Mutex::new(rx),
        }
    }

    /// Executes every task currently queued and returns how many ran.
    pub fn drain(&self) -> usize {
        let rx = lock(&self.rx);
        let mut count = 0;
        while let Ok(task) = rx.try_recv() {
            task();
            count += 1;
        }
        count
    }
}

impl fmt::Debug for MockMainThread {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockMainThread").finish_non_exhaustive()
    }
}

impl MainThread for MockMainThread {
    fn run(&self, task: Task) {
        // A closed / full queue means the runtime is shutting down; dropping
        // the task is the correct outcome — the receiver end owns the mock and
        // simply won't observe further tasks.
        drop(self.tx.send(task));
    }
}
