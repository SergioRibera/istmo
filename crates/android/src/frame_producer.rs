//! What a Kotlin `IstmoFrameSender` holds on the Rust side: a native
//! producer of one sink, the queue that tells Kotlin which of its
//! buffers Rust released, and an optional pool of Rust-owned slots
//! Kotlin writes into.
//!
//! Nothing here calls into the JVM. A frame's release (on whatever
//! thread drops it) only pushes the frame's ticket onto [`Released`];
//! the sender's own Kotlin thread waits there and closes the `Image` or
//! other owner itself. So Rust never holds a JNI reference across
//! threads, and never attaches a thread to release one.

use std::alloc::{self, Layout};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;

use istmo_core::native_frame::{
    BufferIdentity, BytePool, NativeBuffer, NativeProducer, SubmitStatus,
};
use istmo_core::sync::lock;

/// Most slots one pool holds: the free and held sets are `u64` masks.
pub(crate) const MAX_SLOTS: u32 = 64;

/// The tickets of buffers Rust released, waiting for the Kotlin sender
/// to close their owners.
#[derive(Debug, Default)]
pub(crate) struct Released {
    tickets: Mutex<Vec<i64>>,
    ready: Condvar,
}

impl Released {
    pub(crate) fn push(&self, ticket: i64) {
        lock(&self.tickets).push(ticket);
        self.ready.notify_all();
    }

    /// Every released ticket, waiting up to `timeout` for the first one.
    pub(crate) fn wait(&self, timeout: Duration) -> Vec<i64> {
        let (mut tickets, _) = self
            .ready
            .wait_timeout_while(lock(&self.tickets), timeout, |tickets| tickets.is_empty())
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        std::mem::take(&mut *tickets)
    }
}

/// Rust-owned memory Kotlin fills through one `ByteBuffer` per slot
/// (created once, with `NewDirectByteBuffer`) and submits without a
/// copy.
///
/// A slot is in one of three hands: free (in `free`), Kotlin's (in
/// `held`, between `acquire` and `submit`), or a frame's (neither,
/// until the frame drops). Only the hand that has a slot touches its
/// bytes, and every hand-over goes through these atomics (and the
/// sink's lock), so writes in one hand happen before reads in the next.
#[derive(Debug)]
pub(crate) struct SlotPool {
    base: NonNull<u8>,
    layout: Layout,
    slot_size: usize,
    count: u32,
    free: AtomicU64,
    held: AtomicU64,
    generation: u64,
}

// SAFETY: the pool owns its allocation; access to each slot's bytes is
// handed between threads through `free` / `held` and the sink (see the
// type's docs), never shared while being written.
unsafe impl Send for SlotPool {}
// SAFETY: as above; `&SlotPool` only exposes atomics and raw slot
// addresses, whose use is governed by that hand-over.
unsafe impl Sync for SlotPool {}

impl SlotPool {
    /// `count` zeroed slots of `slot_size` bytes; `None` for an empty or
    /// oversized request or a failed allocation.
    pub(crate) fn new(slot_size: usize, count: u32) -> Option<Self> {
        if slot_size == 0 || count == 0 || count > MAX_SLOTS {
            return None;
        }
        let total = slot_size.checked_mul(usize::try_from(count).ok()?)?;
        let layout = Layout::from_size_align(total, 64).ok()?;
        // SAFETY: `layout` has a non-zero size (both factors are > 0).
        let base = NonNull::new(unsafe { alloc::alloc_zeroed(layout) })?;
        let all = if count == MAX_SLOTS {
            u64::MAX
        } else {
            (1u64 << count) - 1
        };
        Some(Self {
            base,
            layout,
            slot_size,
            count,
            free: AtomicU64::new(all),
            held: AtomicU64::new(0),
            generation: BufferIdentity::next_generation(),
        })
    }

    pub(crate) const fn count(&self) -> u32 {
        self.count
    }

    pub(crate) const fn slot_size(&self) -> usize {
        self.slot_size
    }

    /// Start of slot `slot` (`slot < count`).
    pub(crate) fn slot_ptr(&self, slot: u32) -> *mut u8 {
        debug_assert!(slot < self.count);
        // `slot * slot_size` is within the allocation by construction.
        self.base
            .as_ptr()
            .wrapping_add(slot as usize * self.slot_size)
    }

    /// Hand a free slot to Kotlin; `None` when every slot is in use.
    pub(crate) fn acquire(&self) -> Option<u32> {
        let mut free = self.free.load(Ordering::Acquire);
        loop {
            if free == 0 {
                return None;
            }
            let slot = free.trailing_zeros();
            let bit = 1u64 << slot;
            match self.free.compare_exchange_weak(
                free,
                free & !bit,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => {
                    self.held.fetch_or(bit, Ordering::AcqRel);
                    return Some(slot);
                }
                Err(now) => free = now,
            }
        }
    }

    /// Take slot `slot` back from Kotlin to hand it to a frame; `false`
    /// when Kotlin does not hold it (never acquired, or submitted twice).
    fn take_held(&self, slot: u32) -> bool {
        if slot >= self.count {
            return false;
        }
        let bit = 1u64 << slot;
        self.held.fetch_and(!bit, Ordering::AcqRel) & bit != 0
    }

    /// Give slot `slot` back to Kotlin unsubmitted.
    pub(crate) fn give_back(&self, slot: u32) -> bool {
        if !self.take_held(slot) {
            return false;
        }
        self.free.fetch_or(1u64 << slot, Ordering::AcqRel);
        true
    }

    fn release(&self, slot: u32) {
        self.free.fetch_or(1u64 << slot, Ordering::AcqRel);
    }
}

impl Drop for SlotPool {
    fn drop(&mut self) {
        // SAFETY: `base` came from `alloc_zeroed(layout)` and is freed
        // once, here.
        unsafe { alloc::dealloc(self.base.as_ptr(), self.layout) };
    }
}

/// The first `len` bytes of one pool slot, owned by a frame. Dropping it
/// frees the slot for Kotlin's next `acquireSlot`.
#[derive(Debug)]
pub struct PoolSlot {
    pool: Arc<SlotPool>,
    slot: u32,
    len: usize,
}

impl AsRef<[u8]> for PoolSlot {
    fn as_ref(&self) -> &[u8] {
        // SAFETY: the frame owns the slot (see `SlotPool`): nobody writes
        // it while this value lives, `len <= slot_size` was checked at
        // submit, and the pool (the allocation) outlives `self`.
        unsafe { std::slice::from_raw_parts(self.pool.slot_ptr(self.slot), self.len) }
    }
}

impl Drop for PoolSlot {
    fn drop(&mut self) {
        self.pool.release(self.slot);
    }
}

/// The Rust half of one Kotlin `IstmoFrameSender`.
#[derive(Debug)]
pub(crate) struct AndroidProducer {
    producer: NativeProducer,
    released: Arc<Released>,
    pool: OnceLock<Arc<SlotPool>>,
    /// Memory of copied frames, reused once the consumer drops them.
    bytes: BytePool,
}

impl AndroidProducer {
    pub(crate) fn new(producer: NativeProducer) -> Self {
        Self {
            producer,
            released: Arc::new(Released::default()),
            pool: OnceLock::new(),
            bytes: BytePool::default(),
        }
    }

    pub(crate) fn released(&self) -> &Released {
        &self.released
    }

    pub(crate) const fn producer(&self) -> &NativeProducer {
        &self.producer
    }

    /// `buffer`, reporting `ticket` to Kotlin once Rust releases it
    /// (`0`: no owner waits for it).
    pub(crate) fn ticketed(&self, buffer: NativeBuffer, ticket: i64) -> NativeBuffer {
        if ticket == 0 {
            return buffer;
        }
        let released = Arc::clone(&self.released);
        buffer.with_release(move || released.push(ticket))
    }

    /// The pool copied frames take their memory from.
    pub(crate) const fn bytes(&self) -> &BytePool {
        &self.bytes
    }

    /// Submit a copy of `data`.
    pub(crate) fn submit_copy(&self, meta: &[u8], data: &[u8]) -> SubmitStatus {
        self.producer.submit(meta, self.bytes.copy_of(data))
    }

    /// The pool, created on first use with `slot_size` × `count`.
    /// `None` when that is invalid or a pool of another shape exists.
    pub(crate) fn pool(&self, slot_size: usize, count: u32) -> Option<&Arc<SlotPool>> {
        if self.pool.get().is_none() {
            let pool = SlotPool::new(slot_size, count)?;
            let _raced = self.pool.set(Arc::new(pool));
        }
        self.pool
            .get()
            .filter(|pool| pool.slot_size() == slot_size && pool.count() == count)
    }

    pub(crate) fn acquire_slot(&self) -> Option<u32> {
        self.pool.get()?.acquire()
    }

    pub(crate) fn give_back_slot(&self, slot: u32) -> bool {
        self.pool.get().is_some_and(|pool| pool.give_back(slot))
    }

    /// Submit the first `len` bytes of slot `slot` without copying.
    pub(crate) fn submit_slot(&self, slot: u32, len: usize, meta: &[u8]) -> SubmitStatus {
        let Some(pool) = self.pool.get() else {
            return SubmitStatus::Invalid;
        };
        if len > pool.slot_size() || !pool.take_held(slot) {
            return SubmitStatus::Invalid;
        }
        let identity = BufferIdentity::new(pool.generation, slot);
        let frame = PoolSlot {
            pool: Arc::clone(pool),
            slot,
            len,
        };
        self.producer.submit(
            meta,
            NativeBuffer::from_bytes(frame).with_identity(identity),
        )
    }
}

#[cfg(test)]
mod tests {
    use istmo_core::native_frame::SinkPolicy;
    use istmo_core::{Runtime, codec};

    use super::*;

    fn producer(
        rt: &Arc<Runtime>,
        policy: SinkPolicy,
    ) -> (istmo_core::FrameSink<u32>, AndroidProducer) {
        let sink = rt.open_frame_sink::<u32>(policy).expect("open");
        let native = rt
            .open_native_producer(sink.id().raw(), 0)
            .expect("producer");
        (sink, AndroidProducer::new(native))
    }

    #[test]
    fn tickets_come_back_once_rust_lets_go() {
        let rt = Runtime::mock().runtime;
        let (sink, producer) = producer(&rt, SinkPolicy::latest());
        let meta = codec::encode(&1u32).expect("meta");
        let first = producer.ticketed(NativeBuffer::from_bytes(vec![1u8]), 7);
        assert!(producer.producer().submit(&meta, first).is_taken());
        assert_eq!(producer.released().wait(Duration::ZERO), [0i64; 0]);
        let second = producer.ticketed(NativeBuffer::from_bytes(vec![2u8]), 8);
        assert!(producer.producer().submit(&meta, second).is_taken());
        assert_eq!(producer.released().wait(Duration::ZERO), [7], "evicted");
        drop(sink.try_recv());
        assert_eq!(producer.released().wait(Duration::from_secs(1)), [8]);
        // A refused frame comes back as well.
        drop(sink);
        let third = producer.ticketed(NativeBuffer::from_bytes(vec![3u8]), 9);
        assert_eq!(
            producer.producer().submit(&meta, third),
            SubmitStatus::Closed
        );
        assert_eq!(producer.released().wait(Duration::ZERO), [9]);
    }

    #[test]
    fn released_tickets_wake_a_waiting_thread() {
        let released = Arc::new(Released::default());
        let waiter = {
            let released = Arc::clone(&released);
            std::thread::spawn(move || released.wait(Duration::from_secs(5)))
        };
        std::thread::sleep(Duration::from_millis(20));
        released.push(3);
        assert_eq!(waiter.join().expect("waiter"), [3]);
    }

    #[test]
    fn pool_slots_cycle_through_kotlin_and_frames() {
        let rt = Runtime::mock().runtime;
        let (sink, producer) = producer(&rt, SinkPolicy::queue(4));
        assert!(producer.pool(0, 2).is_none());
        assert!(producer.pool(16, MAX_SLOTS + 1).is_none());
        let pool = Arc::clone(producer.pool(16, 2).expect("pool"));
        assert!(producer.pool(32, 2).is_none(), "one shape per producer");
        let meta = codec::encode(&5u32).expect("meta");

        let a = producer.acquire_slot().expect("slot a");
        let b = producer.acquire_slot().expect("slot b");
        assert_eq!(producer.acquire_slot(), None, "both slots out");
        // Kotlin writes slot `a`, then submits it.
        // SAFETY: slot `a` is held by "Kotlin" (this test) right now.
        unsafe { std::ptr::write_bytes(pool.slot_ptr(a), 0xab, 16) };
        assert_eq!(producer.submit_slot(a, 4, &meta), SubmitStatus::Accepted);
        assert_eq!(
            producer.submit_slot(a, 4, &meta),
            SubmitStatus::Invalid,
            "twice"
        );
        assert_eq!(
            producer.submit_slot(b, 17, &meta),
            SubmitStatus::Invalid,
            "too long"
        );
        assert!(producer.give_back_slot(b));
        assert!(!producer.give_back_slot(b));

        let frame = sink.try_recv().expect("frame");
        assert_eq!(frame.buffer.as_bytes(), Some(&[0xab; 4][..]));
        let identity = frame.buffer.identity().expect("identity");
        assert_eq!(identity.slot, a);
        assert_eq!(producer.acquire_slot(), Some(b), "a is still the frame's");
        drop(frame);
        assert_eq!(producer.acquire_slot(), Some(a));
        drop(producer);
        drop(pool);
    }
}
