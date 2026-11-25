//! One shared timer thread for engines without a native timer: decision
//! and scheme-head timeouts become heap entries, not a sleeping thread
//! each.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

type Task = Box<dyn FnOnce() + Send>;

static TIMER: OnceLock<Option<flume::Sender<(Instant, Task)>>> = OnceLock::new();

/// Run `task` on the timer thread once `delay` has passed. Tasks must be
/// short (they post to an engine). Without a timer thread (spawn failed)
/// the task is dropped and a warning logged.
pub(crate) fn after(delay: Duration, task: Task) {
    let sender = TIMER.get_or_init(|| {
        let (tx, rx) = flume::unbounded();
        std::thread::Builder::new()
            .name("istmo-webview-timer".into())
            .spawn(move || run(&rx))
            .ok()
            .map(|_| tx)
    });
    let deadline = Instant::now() + delay;
    match sender {
        Some(tx) if tx.send((deadline, task)).is_ok() => {}
        _ => tracing::warn!("no timer thread: a web view timeout will not fire"),
    }
}

fn run(rx: &flume::Receiver<(Instant, Task)>) {
    let mut heap: BinaryHeap<Reverse<(Instant, u64)>> = BinaryHeap::new();
    let mut tasks: HashMap<u64, Task> = HashMap::new();
    let mut seq = 0_u64;
    loop {
        let next = heap.peek().map(|Reverse((at, _))| *at);
        let received = match next {
            Some(at) => match rx.recv_deadline(at) {
                Ok(entry) => Some(entry),
                Err(flume::RecvTimeoutError::Timeout) => None,
                Err(flume::RecvTimeoutError::Disconnected) => return,
            },
            None => match rx.recv() {
                Ok(entry) => Some(entry),
                Err(_) => return,
            },
        };
        if let Some((at, task)) = received {
            seq += 1;
            heap.push(Reverse((at, seq)));
            tasks.insert(seq, task);
        }
        let now = Instant::now();
        while let Some(Reverse((at, id))) = heap.peek().copied()
            && at <= now
        {
            heap.pop();
            if let Some(task) = tasks.remove(&id) {
                task();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    #[test]
    fn fires_in_deadline_order() {
        let (tx, rx) = flume::unbounded();
        let start = Instant::now();
        for (ms, tag) in [(60_u64, 3), (20, 1), (40, 2)] {
            let tx = tx.clone();
            super::after(
                Duration::from_millis(ms),
                Box::new(move || tx.send(tag).expect("receiver alive")),
            );
        }
        let order: Vec<i32> = (0..3)
            .map(|_| rx.recv_timeout(Duration::from_secs(2)).expect("fired"))
            .collect();
        assert_eq!(order, [1, 2, 3]);
        assert!(start.elapsed() >= Duration::from_millis(60));
    }
}
