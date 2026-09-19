//! "Run this on the platform's main thread" abstraction.
//!
//! Every platform surface (Android UI, iOS main queue, desktop event
//! loops) needs a way to hand a small closure back to the UI thread.
//! [`MainThread`] is that seam; implementations live in the transport
//! crates. Desktop / test code typically wires an [`InlineMainThread`]
//! that runs work on the caller.

use std::fmt;

use flume::{Receiver, Sender, bounded};

use crate::sync::lock;

/// Abstraction over "post this task to the platform's main thread".
pub trait MainThread: Send + Sync + fmt::Debug {
    /// Schedule `task` on the main thread.
    ///
    /// Implementations are free to run the task synchronously
    /// (see [`InlineMainThread`]) or defer it to a real event loop.
    fn run(&self, task: Task);
}

/// Boxed callback dispatched through [`MainThread::run`].
pub type Task = Box<dyn FnOnce() + Send + 'static>;

/// A [`MainThread`] that runs each task synchronously on the calling
/// thread.
///
/// Suitable for desktop binaries, headless services and tests where
/// there is no dedicated UI thread to hop onto.
#[derive(Debug, Default)]
pub struct InlineMainThread;

impl MainThread for InlineMainThread {
    fn run(&self, task: Task) {
        task();
    }
}

/// A [`MainThread`] that buffers tasks in memory for later inspection.
///
/// Used in unit tests to observe or drain scheduled callbacks without a
/// real runloop; call [`MockMainThread::drain`] from the test thread to
/// run every buffered task.
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
    /// Build a mock main thread with the default buffer capacity.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Build a mock main thread with a custom buffer capacity.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        let (tx, rx) = bounded(capacity);
        Self {
            tx,
            rx: std::sync::Mutex::new(rx),
        }
    }

    /// Run every currently buffered task and return the number executed.
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
        drop(self.tx.send(task));
    }
}
