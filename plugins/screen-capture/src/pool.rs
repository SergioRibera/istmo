//! Pool accounting shared by every backend that owns buffers.
//!
//! The rule: **never hand out the last free buffer.** A pooled buffer is
//! delivered only while `in_flight < depth - 1` (in flight = frames in
//! the sink plus frames the consumer holds, clones and leases included),
//! so the producer always has one to render into. A pool of one or less
//! holds nothing.
//!
//! A [`SlotLease`] travels with each delivered buffer as its release
//! hook; dropping the last clone of the buffer returns the slot. Leases
//! from an older pool generation are inert: the reallocation already
//! forgot them.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};

use crate::types::BufferIdentity;

/// The pool refused a hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Exhausted {
    /// The first refusal since the last successful hold: the start of a
    /// new episode, which the session reports once.
    pub new_episode: bool,
    /// Episodes so far, this one included.
    pub episode: u32,
}

#[derive(Debug)]
struct State {
    depth: usize,
    generation: u64,
    /// Holds per slot of the current generation.
    held: Vec<u32>,
    in_flight: usize,
    exhausted: bool,
    episodes: u32,
}

impl State {
    const fn limit(&self) -> usize {
        self.depth.saturating_sub(1)
    }
}

#[derive(Debug)]
struct Inner {
    state: Mutex<State>,
    /// Woken (best effort) whenever a hold returns, so a stalled
    /// producer can pull again.
    released_tx: flume::Sender<()>,
    released_rx: flume::Receiver<()>,
}

impl Inner {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Every critical section is a few integer updates; a poisoned
        // lock still holds consistent counters.
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Accounting for one producer pool. Cheap to clone; clones share it.
#[derive(Debug, Clone)]
pub struct PoolAccounting {
    inner: Arc<Inner>,
}

impl PoolAccounting {
    /// A pool of `depth` buffers, generation 1.
    #[must_use]
    pub fn new(depth: usize) -> Self {
        let (released_tx, released_rx) = flume::bounded(1);
        Self {
            inner: Arc::new(Inner {
                state: Mutex::new(State {
                    depth,
                    generation: 1,
                    held: Vec::new(),
                    in_flight: 0,
                    exhausted: false,
                    episodes: 0,
                }),
                released_tx,
                released_rx,
            }),
        }
    }

    /// Buffers the consumer may hold at once.
    #[must_use]
    pub fn limit(&self) -> usize {
        self.inner.lock().limit()
    }

    #[must_use]
    pub fn depth(&self) -> usize {
        self.inner.lock().depth
    }

    #[must_use]
    pub fn in_flight(&self) -> usize {
        self.inner.lock().in_flight
    }

    #[must_use]
    pub fn generation(&self) -> u64 {
        self.inner.lock().generation
    }

    /// Exhaustion episodes so far.
    #[must_use]
    pub fn episodes(&self) -> u32 {
        self.inner.lock().episodes
    }

    /// Whether `slot` of the current generation is out with the
    /// consumer (the producer must not render into it).
    #[must_use]
    pub fn is_held(&self, slot: u32) -> bool {
        let state = self.inner.lock();
        usize::try_from(slot)
            .ok()
            .and_then(|i| state.held.get(i))
            .is_some_and(|n| *n > 0)
    }

    /// Whether a hold would be granted now.
    #[must_use]
    pub fn has_room(&self) -> bool {
        let state = self.inner.lock();
        state.in_flight < state.limit()
    }

    /// Hold `slot` for one delivered frame.
    ///
    /// # Errors
    /// [`Exhausted`] when the consumer already holds every buffer it may.
    pub fn hold(&self, slot: u32) -> Result<SlotLease, Exhausted> {
        let mut state = self.inner.lock();
        if state.in_flight >= state.limit() {
            let new_episode = !state.exhausted;
            if new_episode {
                state.exhausted = true;
                state.episodes = state.episodes.saturating_add(1);
            }
            return Err(Exhausted {
                new_episode,
                episode: state.episodes,
            });
        }
        state.exhausted = false;
        let index = usize::try_from(slot).unwrap_or(usize::MAX);
        if index != usize::MAX {
            if state.held.len() <= index {
                state.held.resize(index + 1, 0);
            }
            if let Some(count) = state.held.get_mut(index) {
                *count = count.saturating_add(1);
            }
        }
        state.in_flight += 1;
        Ok(SlotLease {
            pool: Arc::clone(&self.inner),
            identity: BufferIdentity {
                pool_generation: state.generation,
                slot,
            },
        })
    }

    /// The pool was reallocated (size, format, modifier, device): bump
    /// the generation and forget every hold. Late releases of older
    /// leases become inert.
    #[must_use = "the new generation tags the reallocated buffers"]
    pub fn reallocate(&self, depth: usize) -> u64 {
        let mut state = self.inner.lock();
        state.generation += 1;
        state.depth = depth;
        state.held.clear();
        state.in_flight = 0;
        state.exhausted = false;
        state.generation
    }

    /// The producer changed its pool size without reallocating the
    /// buffers still out: the limit follows at once, even below the
    /// current holds (new holds wait until enough come back).
    pub fn resize(&self, depth: usize) {
        self.inner.lock().depth = depth;
    }

    /// A channel that receives a unit (coalesced) whenever a hold
    /// returns.
    #[must_use]
    pub fn released(&self) -> flume::Receiver<()> {
        self.inner.released_rx.clone()
    }
}

/// One hold on a pooled slot. Drop it (usually as a `NativeBuffer`
/// release hook) to return the slot.
#[derive(Debug)]
pub struct SlotLease {
    pool: Arc<Inner>,
    identity: BufferIdentity,
}

impl SlotLease {
    /// The buffer this lease holds.
    #[must_use]
    pub const fn identity(&self) -> BufferIdentity {
        self.identity
    }
}

impl Drop for SlotLease {
    fn drop(&mut self) {
        {
            let mut state = self.pool.lock();
            if state.generation != self.identity.pool_generation {
                return;
            }
            let index = usize::try_from(self.identity.slot).unwrap_or(usize::MAX);
            if let Some(count) = state.held.get_mut(index) {
                *count = count.saturating_sub(1);
            }
            state.in_flight = state.in_flight.saturating_sub(1);
        }
        // Full means a wake-up is already pending.
        let _pending = self.pool.released_tx.try_send(());
    }
}

#[cfg(test)]
mod tests {
    use istmo_core::NativeBuffer;

    use super::*;

    #[test]
    fn holds_all_but_one_buffer() {
        let pool = PoolAccounting::new(4);
        let mut held = Vec::new();
        for slot in 0..3 {
            held.push(pool.hold(slot).expect("room"));
        }
        let refused = pool
            .hold(3)
            .expect_err("the last free buffer stays with the producer");
        assert!(refused.new_episode);
        assert_eq!(held.len(), 3);
        assert_eq!(pool.in_flight(), 3);
    }

    #[test]
    fn tiny_or_unknown_pool_holds_nothing() {
        for depth in [0, 1] {
            let pool = PoolAccounting::new(depth);
            assert!(pool.hold(0).is_err(), "depth {depth}");
            assert!(!pool.has_room());
        }
    }

    #[test]
    fn release_returns_each_hold_once() {
        let pool = PoolAccounting::new(3);
        let a = pool.hold(0).expect("room");
        let b = pool.hold(0).expect("room");
        assert!(pool.is_held(0));
        drop(a);
        assert!(pool.is_held(0), "one hold is still out");
        drop(b);
        assert!(!pool.is_held(0));
        assert_eq!(pool.in_flight(), 0);
    }

    #[test]
    fn removed_buffers_make_late_releases_inert() {
        let pool = PoolAccounting::new(3);
        let old = pool.hold(1).expect("room");
        let generation = pool.reallocate(3);
        assert_eq!(generation, 2);
        let fresh = pool.hold(1).expect("room");
        drop(old);
        assert!(pool.is_held(1), "the old lease must not free the new slot");
        assert_eq!(pool.in_flight(), 1);
        assert_eq!(fresh.identity().pool_generation, 2);
    }

    #[test]
    fn shrinking_pool_lowers_the_limit_below_current_holds() {
        let pool = PoolAccounting::new(5);
        let mut held = Vec::new();
        for slot in 0..4 {
            held.push(pool.hold(slot).expect("room"));
        }
        pool.resize(3);
        assert!(pool.hold(9).is_err());
        let mut held = held.into_iter();
        drop(held.next());
        assert!(pool.hold(9).is_err(), "3 still out, limit is 2");
        drop(held.next());
        assert!(pool.hold(9).is_err(), "2 still out, limit is 2");
        drop(held.next());
        assert!(pool.hold(9).is_ok());
    }

    #[test]
    fn an_episode_is_reported_once() {
        let pool = PoolAccounting::new(2);
        let lease = pool.hold(0).expect("room");
        assert!(pool.hold(1).expect_err("full").new_episode);
        assert!(!pool.hold(1).expect_err("full").new_episode);
        drop(lease);
        let _lease = pool.hold(0).expect("room");
        let again = pool.hold(1).expect_err("full");
        assert!(again.new_episode);
        assert_eq!(again.episode, 2);
    }

    #[test]
    fn lease_is_held_by_every_frame_clone() {
        let pool = PoolAccounting::new(3);
        let lease = pool.hold(0).expect("room");
        let buffer = NativeBuffer::from_bytes(vec![0u8; 4]).with_release(move || drop(lease));
        let clone = buffer.clone();
        drop(buffer);
        assert!(pool.is_held(0));
        drop(clone);
        assert!(!pool.is_held(0));
        assert!(pool.released().try_recv().is_ok(), "the producer is woken");
    }

    #[test]
    fn release_from_another_thread_frees_the_slot() {
        let pool = PoolAccounting::new(3);
        let lease = pool.hold(2).expect("room");
        std::thread::spawn(move || drop(lease))
            .join()
            .expect("release thread");
        assert!(!pool.is_held(2));
        assert_eq!(pool.in_flight(), 0);
    }
}
