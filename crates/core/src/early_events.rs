//! Buffers for values produced before their first subscriber attaches.
//!
//! Cold-start deep links, launch-intent notifications and initial
//! lifecycle state all need to reach the app even when no consumer is
//! subscribed yet. Two retention shapes are provided:
//!
//! - [`LatestValueSlot`] — keeps only the most recent value; late
//!   subscribers see it on first attach followed by live updates.
//! - [`PreMainQueue`] — bounded FIFO; buffered values are drained into
//!   the first subscriber in publish order.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use flume::{Receiver, Sender, unbounded};

use crate::sync::lock;

/// "Latest value wins" retention slot.
///
/// Publishes fan out to every current subscriber; late subscribers get
/// the most recent value first and then live updates.
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
    /// Build an empty slot.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Publish a new value and forward it to every subscriber.
    ///
    /// Dead subscribers (whose receiver was dropped) are cleaned up
    /// during the fan-out.
    pub fn publish(&self, value: Vec<u8>) {
        let mut inner = lock(&self.inner);
        inner
            .subscribers
            .retain(|tx| tx.send(value.clone()).is_ok());
        inner.value = Some(value);
    }

    /// Subscribe to future updates.
    ///
    /// The returned receiver is primed with the most recently published
    /// value, if any, followed by live updates.
    pub fn subscribe(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = unbounded();
        let mut inner = lock(&self.inner);
        if let Some(current) = inner.value.as_ref() {
            drop(tx.send(current.clone()));
        }
        inner.subscribers.push(tx);
        rx
    }

    /// Return a copy of the currently retained value, if any.
    #[must_use]
    pub fn peek(&self) -> Option<Vec<u8>> {
        lock(&self.inner).value.clone()
    }
}

/// Bounded FIFO retention.
///
/// While no subscriber is attached, publishes accumulate up to
/// `capacity` entries — the oldest is dropped once the queue overflows.
/// The first subscriber drains the buffered items in publish order;
/// subsequent publishes fan out live to every attached subscriber.
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
    /// Build a queue that retains up to `capacity` items while no
    /// subscriber is attached.
    ///
    /// `capacity == 0` disables buffering entirely — publishes without a
    /// live subscriber are dropped.
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

    /// Publish a value.
    ///
    /// Delivered directly to every attached subscriber; if none is
    /// attached, it is appended to the internal buffer (dropping the
    /// oldest entry on overflow).
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

    /// Subscribe to the queue.
    ///
    /// The receiver is primed with every buffered value in publish
    /// order and then receives live updates.
    pub fn subscribe(&self) -> Receiver<Vec<u8>> {
        let (tx, rx) = unbounded();
        let mut inner = lock(&self.inner);
        for buffered in inner.buffered.drain(..) {
            drop(tx.send(buffered));
        }
        inner.subscribers.push(tx);
        rx
    }
}

/// Named directory of early-event slots and queues.
///
/// Slots are addressed by string key — usually a plugin id or a
/// subchannel path (`"lifecycle"`, `"deep-links"`, and so on).
#[derive(Debug, Default)]
pub struct EarlyEventStore {
    latest: Mutex<HashMap<String, Arc<LatestValueSlot>>>,
    queues: Mutex<HashMap<String, Arc<PreMainQueue>>>,
}

impl EarlyEventStore {
    /// Build an empty store.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the [`LatestValueSlot`] for `key`, creating it on first
    /// access.
    pub fn latest_slot(&self, key: &str) -> Arc<LatestValueSlot> {
        let mut map = lock(&self.latest);
        if let Some(existing) = map.get(key) {
            return existing.clone();
        }
        let slot = Arc::new(LatestValueSlot::new());
        map.insert(key.to_owned(), slot.clone());
        slot
    }

    /// Return the [`PreMainQueue`] for `key`, creating it on first
    /// access with the given `capacity`.
    ///
    /// If a queue already exists under `key`, its existing capacity is
    /// preserved and the `capacity` argument is ignored.
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
