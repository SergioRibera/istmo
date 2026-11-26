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

/// The pool is full: the consumer holds every buffer it may.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Exhausted {
    /// The first refusal since the last successful hold: the start of a
    /// new episode, which the session reports once.
    pub(crate) new_episode: bool,
    /// Episodes so far, this one included.
    pub(crate) episode: u32,
}

/// Why [`PoolAccounting::hold`] refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum HoldError {
    /// The consumer already holds every buffer it may.
    Exhausted(Exhausted),
    /// `slot` is not a slot of this pool (`slot >= depth`): a producer
    /// bug, never a reason to grow the accounting.
    SlotOutOfRange {
        /// The refused slot.
        slot: u32,
        /// The pool depth when it was refused.
        depth: usize,
    },
}

impl std::fmt::Display for HoldError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exhausted(_) => f.write_str("every buffer the consumer may hold is out"),
            Self::SlotOutOfRange { slot, depth } => {
                write!(f, "slot {slot} is outside a pool of {depth}")
            }
        }
    }
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

    /// Records the refusal when the pool is full, under the same lock as
    /// the check.
    const fn refuse_if_full(&mut self) -> Result<(), Exhausted> {
        if self.in_flight < self.limit() {
            return Ok(());
        }
        let new_episode = !self.exhausted;
        if new_episode {
            self.exhausted = true;
            self.episodes = self.episodes.saturating_add(1);
        }
        Err(Exhausted {
            new_episode,
            episode: self.episodes,
        })
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
pub(crate) struct PoolAccounting {
    inner: Arc<Inner>,
}

impl PoolAccounting {
    /// A pool of `depth` buffers, generation 1.
    #[must_use]
    pub(crate) fn new(depth: usize) -> Self {
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

    #[cfg(test)]
    #[must_use]
    pub(crate) fn in_flight(&self) -> usize {
        self.inner.lock().in_flight
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn generation(&self) -> u64 {
        self.inner.lock().generation
    }

    /// Exhaustion episodes so far.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn episodes(&self) -> u32 {
        self.inner.lock().episodes
    }

    /// Whether `slot` of the current generation is out with the
    /// consumer (the producer must not render into it).
    #[must_use]
    pub(crate) fn is_held(&self, slot: u32) -> bool {
        let state = self.inner.lock();
        usize::try_from(slot)
            .ok()
            .and_then(|i| state.held.get(i))
            .is_some_and(|n| *n > 0)
    }

    /// Whether a hold would be granted now, without recording anything.
    #[cfg(test)]
    #[must_use]
    pub(crate) fn has_room(&self) -> bool {
        let state = self.inner.lock();
        state.in_flight < state.limit()
    }

    /// `Ok` while a hold would be granted; otherwise the refusal,
    /// recorded as part of the current exhaustion episode. Check and
    /// record happen under one lock, so a release racing with the call
    /// can never turn it into a hold.
    ///
    /// # Errors
    /// [`Exhausted`] when the consumer already holds every buffer it may.
    pub(crate) fn check_room(&self) -> Result<(), Exhausted> {
        self.inner.lock().refuse_if_full()
    }

    /// Hold `slot` for one delivered frame.
    ///
    /// # Errors
    /// [`HoldError::Exhausted`] when the consumer already holds every
    /// buffer it may, [`HoldError::SlotOutOfRange`] when `slot` is not
    /// below the pool depth.
    pub(crate) fn hold(&self, slot: u32) -> Result<SlotLease, HoldError> {
        let mut state = self.inner.lock();
        state.refuse_if_full().map_err(HoldError::Exhausted)?;
        let depth = state.depth;
        let index = usize::try_from(slot)
            .ok()
            .filter(|index| *index < depth)
            .ok_or(HoldError::SlotOutOfRange { slot, depth })?;
        state.exhausted = false;
        if state.held.len() <= index {
            // Bounded by the depth checked above.
            state.held.resize(index + 1, 0);
        }
        if let Some(count) = state.held.get_mut(index) {
            *count = count.saturating_add(1);
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
    pub(crate) fn reallocate(&self, depth: usize) -> u64 {
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
    #[cfg(test)]
    pub(crate) fn resize(&self, depth: usize) {
        self.inner.lock().depth = depth;
    }

    /// A channel that receives a unit (coalesced) whenever a hold
    /// returns.
    #[must_use]
    pub(crate) fn released(&self) -> flume::Receiver<()> {
        self.inner.released_rx.clone()
    }
}

/// One hold on a pooled slot. Drop it (usually as a `NativeBuffer`
/// release hook) to return the slot.
#[derive(Debug)]
pub(crate) struct SlotLease {
    pool: Arc<Inner>,
    identity: BufferIdentity,
}

impl SlotLease {
    /// The buffer this lease holds.
    #[must_use]
    pub(crate) const fn identity(&self) -> BufferIdentity {
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

    fn refused(result: Result<SlotLease, HoldError>) -> Exhausted {
        match result.expect_err("refused") {
            HoldError::Exhausted(exhausted) => exhausted,
            other @ HoldError::SlotOutOfRange { .. } => panic!("not exhaustion: {other}"),
        }
    }

    #[test]
    fn holds_all_but_one_buffer() {
        let pool = PoolAccounting::new(4);
        let mut held = Vec::new();
        for slot in 0..3 {
            held.push(pool.hold(slot).expect("room"));
        }
        let refused = refused(pool.hold(3));
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
        assert!(pool.hold(2).is_err());
        let mut held = held.into_iter();
        drop(held.next());
        assert!(pool.hold(2).is_err(), "3 still out, limit is 2");
        drop(held.next());
        assert!(pool.hold(2).is_err(), "2 still out, limit is 2");
        drop(held.next());
        assert!(pool.hold(2).is_ok());
    }

    #[test]
    fn an_episode_is_reported_once() {
        let pool = PoolAccounting::new(2);
        let lease = pool.hold(0).expect("room");
        assert!(refused(pool.hold(1)).new_episode);
        assert!(!refused(pool.hold(1)).new_episode);
        drop(lease);
        let _lease = pool.hold(0).expect("room");
        let again = refused(pool.hold(1));
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

    #[test]
    fn slots_beyond_the_depth_are_refused_without_growing() {
        let pool = PoolAccounting::new(3);
        assert_eq!(
            pool.hold(u32::MAX).expect_err("not a slot of this pool"),
            HoldError::SlotOutOfRange {
                slot: u32::MAX,
                depth: 3
            }
        );
        assert_eq!(pool.in_flight(), 0);
        assert!(!pool.is_held(u32::MAX));
    }

    #[test]
    fn check_room_records_the_episode_and_never_holds() {
        let pool = PoolAccounting::new(2);
        assert!(pool.check_room().is_ok());
        assert_eq!(pool.in_flight(), 0, "a probe is not a hold");
        let lease = pool.hold(0).expect("room");
        let first = pool.check_room().expect_err("full");
        assert!(first.new_episode);
        assert!(!pool.check_room().expect_err("full").new_episode);
        // A release between a check and the next one only reopens room.
        drop(lease);
        assert!(pool.check_room().is_ok());
        assert_eq!(pool.in_flight(), 0);
        assert_eq!(pool.episodes(), 1);
    }

    #[test]
    fn racing_releases_never_turn_a_probe_into_a_hold() {
        let pool = PoolAccounting::new(3);
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let releaser = {
            let pool = pool.clone();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                    if let Ok(lease) = pool.hold(0) {
                        drop(lease);
                    }
                }
            })
        };
        for _ in 0..20_000 {
            let _probe = pool.check_room();
            assert!(pool.in_flight() <= 2);
        }
        stop.store(true, std::sync::atomic::Ordering::Relaxed);
        releaser.join().expect("releaser");
        assert_eq!(pool.in_flight(), 0);
    }
}
