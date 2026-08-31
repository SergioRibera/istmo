//! Main-thread dispatcher implementations.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use istmo_core::{InlineMainThread, MainThread, MockMainThread};

#[test]
fn inline_main_thread_runs_immediately() {
    let counter = Arc::new(AtomicUsize::new(0));
    let counter_task = counter.clone();
    InlineMainThread.run(Box::new(move || {
        counter_task.fetch_add(1, Ordering::Relaxed);
    }));
    assert_eq!(counter.load(Ordering::Relaxed), 1);
}

#[test]
fn mock_main_thread_defers_until_drain() {
    let dispatcher = MockMainThread::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..3 {
        let counter_task = counter.clone();
        dispatcher.run(Box::new(move || {
            counter_task.fetch_add(1, Ordering::Relaxed);
        }));
    }
    assert_eq!(counter.load(Ordering::Relaxed), 0);
    assert_eq!(dispatcher.drain(), 3);
    assert_eq!(counter.load(Ordering::Relaxed), 3);
    // Nothing left to drain on the next call.
    assert_eq!(dispatcher.drain(), 0);
}
