//! Early-event primitives.
//!
//! Two primitives cover the "events that happen before the plugin subscribes"
//! problem stated in the PLAN:
//!
//! * [`LatestValueSlot`] — value-semantics: a late subscriber immediately
//!   observes the latest published value, then any updates that follow.
//!   Ideal for lifecycle state (foreground / background / low-memory).
//! * [`PreMainQueue`] — bounded FIFO: values published before there is a
//!   subscriber are buffered and drained into the first subscriber, then
//!   later publications are forwarded live. Ideal for launch-intent
//!   deep-links and push notifications delivered before the app is ready.
//!
//! Both primitives operate on opaque byte payloads. Plugins own the codec.
//!
//! The [`EarlyEventStore`] aggregates instances of both primitives by string
//! key so a single runtime can host arbitrarily many independent event lanes
//! without knowing their domain types.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender, unbounded};

use crate::sync::lock;

/// A slot that retains the most recent published value and replays it to any
/// new subscriber.
#[derive(Debug, Default)]
pub struct LatestValueSlot {
    inner: Mutex<LatestInner>,
}

#[derive(Debug, Default)]
struct LatestInner {
    value: Option<Vec<u8>>,
    subscribers: Vec<Sender<Vec<u8>>>,
}

impl LatestValueSlot {
    /// Creates an empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publishes a new value, replacing any previously retained one, and
    /// forwards the value to all live subscribers.
    pub fn publish(&self, value: Vec<u8>) {
        let mut inner = lock(&self.inner);
        inner
            .subscribers
            .retain(|tx| tx.send(value.clone()).is_ok());
        inner.value = Some(value);
    }

    /// Subscribes to updates. The returned receiver first yields the currently
    /// retained value (if any), then every subsequent [`publish`] call.
    ///
    /// [`publish`]: Self::publish
    pub fn subscribe(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = unbounded();
        let mut inner = lock(&self.inner);
        if let Some(current) = inner.value.as_ref() {
            // If the receiver has already been dropped there is nothing to do.
            drop(tx.send(current.clone()));
        }
        inner.subscribers.push(tx);
        rx
    }

    /// Snapshot of the retained value, if any.
    #[must_use]
    pub fn peek(&self) -> Option<Vec<u8>> {
        lock(&self.inner).value.clone()
    }
}

/// Bounded FIFO that buffers events published before a subscriber exists.
///
/// A publication is buffered when there are zero live subscribers. Once a
/// subscriber attaches, the entire buffer is drained into it and further
/// publications are broadcast live to all live subscribers.
///
/// When the buffer is full, the oldest entry is dropped to make room —
/// matching the "short queue of pre-main deep links" spec in the PLAN.
#[derive(Debug)]
pub struct PreMainQueue {
    inner: Mutex<PreMainInner>,
    capacity: usize,
}

#[derive(Debug)]
struct PreMainInner {
    buffered: VecDeque<Vec<u8>>,
    subscribers: Vec<Sender<Vec<u8>>>,
}

impl PreMainQueue {
    /// Creates a queue with the given buffer capacity. `capacity` of zero
    /// disables buffering entirely (publications with no subscriber are dropped).
    #[must_use]
    pub const fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(PreMainInner {
                buffered: VecDeque::new(),
                subscribers: Vec::new(),
            }),
            capacity,
        }
    }

    /// Publishes a value. Buffered if there are no subscribers yet, otherwise
    /// broadcast live.
    pub fn publish(&self, value: Vec<u8>) {
        let mut inner = lock(&self.inner);
        if inner.subscribers.is_empty() {
            if self.capacity == 0 {
                return;
            }
            if inner.buffered.len() == self.capacity {
                inner.buffered.pop_front();
            }
            inner.buffered.push_back(value);
        } else {
            inner
                .subscribers
                .retain(|tx| tx.send(value.clone()).is_ok());
        }
    }

    /// Subscribes to the queue. On first subscribe the buffer is drained into
    /// the new subscriber; subsequent subscribers receive only live events.
    pub fn subscribe(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = unbounded();
        let mut inner = lock(&self.inner);
        // Drain buffered events into the new subscriber. `drain(..)` empties
        // the buffer regardless of how many subscribers already exist, which
        // preserves at-least-once delivery for a single subscriber path.
        for buffered in inner.buffered.drain(..) {
            drop(tx.send(buffered));
        }
        inner.subscribers.push(tx);
        rx
    }
}

/// Registry of early-event lanes keyed by string.
///
/// The store is kept in the [`Runtime`](crate::runtime::Runtime) as pure
/// plumbing; the actual channel names are chosen by plugins.
#[derive(Debug, Default)]
pub struct EarlyEventStore {
    latest: Mutex<HashMap<String, Arc<LatestValueSlot>>>,
    queues: Mutex<HashMap<String, Arc<PreMainQueue>>>,
}

impl EarlyEventStore {
    /// Creates an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the latest-value slot for `key`, creating it if absent.
    pub fn latest_slot(&self, key: &str) -> Arc<LatestValueSlot> {
        let mut map = lock(&self.latest);
        if let Some(existing) = map.get(key) {
            return existing.clone();
        }
        let slot = Arc::new(LatestValueSlot::new());
        map.insert(key.to_owned(), slot.clone());
        slot
    }

    /// Returns the pre-main queue for `key`, creating it with `capacity` if
    /// absent. If a queue already exists, its previously-configured capacity
    /// is preserved and `capacity` is ignored.
    pub fn queue(&self, key: &str, capacity: usize) -> Arc<PreMainQueue> {
        let mut map = lock(&self.queues);
        if let Some(existing) = map.get(key) {
            return existing.clone();
        }
        let queue = Arc::new(PreMainQueue::new(capacity));
        map.insert(key.to_owned(), queue.clone());
        queue
    }
}
