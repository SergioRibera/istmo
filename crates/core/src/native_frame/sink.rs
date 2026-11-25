//! [`FrameSink`] / [`FrameSender`]: the bounded, never-blocking
//! mailbox between a producer and one consumer.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::num::NonZeroUsize;
use std::sync::{Arc, Mutex, Weak};

use flume::{Receiver, Sender};

use super::{FrameSinkId, NativeBuffer};
use crate::codec;
use crate::message::Message;
use crate::runtime::Runtime;
use crate::sync::lock;

/// What a full sink does with one more frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Overflow {
    /// Evict the oldest queued frame and take the new one.
    #[default]
    DropOldest,
    /// Refuse the new frame and hand it back to the producer.
    DropNewest,
}

/// How many frames a sink queues and what happens past that.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SinkPolicy {
    depth: NonZeroUsize,
    overflow: Overflow,
}

impl SinkPolicy {
    /// Keep only the newest frame: a new frame replaces a queued one.
    #[must_use]
    pub const fn latest() -> Self {
        Self {
            depth: NonZeroUsize::MIN,
            overflow: Overflow::DropOldest,
        }
    }

    /// Queue up to `depth` frames, evicting the oldest when full.
    #[must_use]
    pub const fn queue(depth: NonZeroUsize) -> Self {
        Self {
            depth,
            overflow: Overflow::DropOldest,
        }
    }

    /// The same depth with another overflow behaviour.
    #[must_use]
    pub const fn with_overflow(self, overflow: Overflow) -> Self {
        Self { overflow, ..self }
    }

    /// Most frames queued at once.
    #[must_use]
    pub const fn depth(self) -> NonZeroUsize {
        self.depth
    }

    /// What a full sink does.
    #[must_use]
    pub const fn overflow(self) -> Overflow {
        self.overflow
    }
}

impl Default for SinkPolicy {
    fn default() -> Self {
        Self::latest()
    }
}

/// One frame: plugin-defined metadata plus the buffer holding the data.
///
/// Dropping it releases the buffer to its producer (unless a clone of
/// the buffer lives on).
#[derive(Debug)]
pub struct NativeFrame<M> {
    /// Producer-defined description of the frame (size, format,
    /// timestamps, …).
    pub meta: M,
    /// The data.
    pub buffer: NativeBuffer,
    seq: u64,
    dropped_before: u64,
}

impl<M> NativeFrame<M> {
    /// Position of the frame among every frame the sink accepted,
    /// counting from 0. Gaps are frames dropped by the policy.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Frames the sink dropped since the previous delivered frame.
    #[must_use]
    pub const fn dropped_before(&self) -> u64 {
        self.dropped_before
    }

    /// Split into metadata and buffer.
    #[must_use]
    pub fn into_parts(self) -> (M, NativeBuffer) {
        (self.meta, self.buffer)
    }
}

/// Lifetime counters of a sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SinkStats {
    /// Frames the sink took in.
    pub accepted: u64,
    /// Frames handed to the consumer.
    pub delivered: u64,
    /// Frames lost to the overflow policy (evicted or refused).
    pub dropped: u64,
}

/// What [`FrameSender::push`] did with an accepted frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pushed {
    /// Queued; nothing was lost.
    Queued,
    /// Queued after evicting the oldest queued frame.
    Displaced,
}

/// A frame the sink did not take, given back to the producer.
pub enum PushError<M> {
    /// The sink is full and refuses new frames ([`Overflow::DropNewest`]).
    Full(M, NativeBuffer),
    /// The consumer is gone or a producer closed the sink: stop
    /// producing.
    Closed(M, NativeBuffer),
}

impl<M> PushError<M> {
    /// The refused metadata and buffer.
    #[must_use]
    pub fn into_parts(self) -> (M, NativeBuffer) {
        match self {
            Self::Full(meta, buffer) | Self::Closed(meta, buffer) => (meta, buffer),
        }
    }

    /// Whether the sink is closed for good.
    #[must_use]
    pub const fn is_closed(&self) -> bool {
        matches!(self, Self::Closed(..))
    }
}

impl<M> fmt::Debug for PushError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(_, buffer) => f.debug_tuple("Full").field(buffer).finish(),
            Self::Closed(_, buffer) => f.debug_tuple("Closed").field(buffer).finish(),
        }
    }
}

impl<M> fmt::Display for PushError<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Full(..) => "frame sink full; frame refused",
            Self::Closed(..) => "frame sink closed",
        })
    }
}

impl<M> std::error::Error for PushError<M> {}

/// Outcome of a native submit
/// ([`Runtime::submit_native_frame`](crate::Runtime::submit_native_frame)),
/// as the integer the transports return to Kotlin and Swift.
///
/// For every status but [`Accepted`](Self::Accepted) and
/// [`Displaced`](Self::Displaced), Rust kept nothing: the buffer it was
/// handed is already released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
pub enum SubmitStatus {
    /// Queued.
    Accepted = 0,
    /// Queued after evicting an older frame.
    Displaced = 1,
    /// The consumer is gone or the sink was closed: stop producing.
    Closed = 2,
    /// No sink with this id in this runtime (stale, or opened by
    /// another process).
    UnknownSink = 3,
    /// The metadata bytes are not the sink's metadata type.
    BadMeta = 4,
    /// The platform cannot do this (e.g. `HardwareBuffer` below API 26).
    Unsupported = 5,
    /// No runtime is running.
    NotStarted = 6,
    /// A malformed argument (null object, negative length).
    Invalid = 7,
    /// The sink is full and refuses new frames.
    Full = 8,
}

impl SubmitStatus {
    /// The integer native callers see.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// Whether the sink kept the frame.
    #[must_use]
    pub const fn is_taken(self) -> bool {
        matches!(self, Self::Accepted | Self::Displaced)
    }
}

impl From<Pushed> for SubmitStatus {
    fn from(pushed: Pushed) -> Self {
        match pushed {
            Pushed::Queued => Self::Accepted,
            Pushed::Displaced => Self::Displaced,
        }
    }
}

struct State<M> {
    queue: VecDeque<NativeFrame<M>>,
    /// No frame is accepted any more; queued ones are still delivered.
    closed: bool,
    next_seq: u64,
    pending_drops: u64,
    stats: SinkStats,
}

/// What a pop found.
enum Pop<M> {
    Frame(NativeFrame<M>),
    Empty,
    Ended,
}

/// The sink itself, shared by the consumer end, the producer ends and
/// the runtime's registry.
pub(super) struct Shared<M> {
    id: u64,
    policy: SinkPolicy,
    state: Mutex<State<M>>,
    /// Capacity 1: a pending token means "look again".
    doorbell: Sender<()>,
}

impl<M> Shared<M> {
    fn push(&self, meta: M, buffer: NativeBuffer) -> Result<Pushed, PushError<M>> {
        let evicted;
        {
            let mut state = lock(&self.state);
            if state.closed {
                return Err(PushError::Closed(meta, buffer));
            }
            evicted = if state.queue.len() < self.policy.depth.get() {
                None
            } else {
                state.pending_drops += 1;
                state.stats.dropped += 1;
                match self.policy.overflow {
                    Overflow::DropOldest => state.queue.pop_front(),
                    Overflow::DropNewest => return Err(PushError::Full(meta, buffer)),
                }
            };
            let seq = state.next_seq;
            state.next_seq += 1;
            state.stats.accepted += 1;
            state.queue.push_back(NativeFrame {
                meta,
                buffer,
                seq,
                dropped_before: 0,
            });
        }
        self.ring();
        // Released outside the lock: a release may call into JNI or
        // CoreFoundation.
        let pushed = if evicted.is_some() {
            Pushed::Displaced
        } else {
            Pushed::Queued
        };
        drop(evicted);
        Ok(pushed)
    }

    fn pop(&self) -> Pop<M> {
        let mut state = lock(&self.state);
        if let Some(mut frame) = state.queue.pop_front() {
            frame.dropped_before = std::mem::take(&mut state.pending_drops);
            state.stats.delivered += 1;
            return Pop::Frame(frame);
        }
        if state.closed { Pop::Ended } else { Pop::Empty }
    }

    fn close(&self) {
        lock(&self.state).closed = true;
        self.ring();
    }

    /// Close and release every queued frame (consumer gone, shutdown).
    fn close_and_drain(&self) {
        let queued = {
            let mut state = lock(&self.state);
            state.closed = true;
            std::mem::take(&mut state.queue)
        };
        drop(queued);
        self.ring();
    }

    fn is_closed(&self) -> bool {
        lock(&self.state).closed
    }

    fn stats(&self) -> SinkStats {
        lock(&self.state).stats
    }

    fn ring(&self) {
        // Full means a wake-up is already pending; disconnected means
        // the consumer is gone. Neither needs another token.
        let _ = self.doorbell.try_send(());
    }
}

/// Type-erased view of a sink, for the registry and native submits.
pub(super) trait ErasedSink: Send + Sync {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn submit(&self, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus;
    fn close(&self);
    fn close_and_drain(&self);
}

impl<M: Message + Send + 'static> ErasedSink for Shared<M> {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn submit(&self, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus {
        let meta = match codec::decode_exact::<M>(meta) {
            Ok(meta) => meta,
            Err(err) => {
                tracing::warn!(%err, sink = self.id, "native frame metadata does not decode");
                return SubmitStatus::BadMeta;
            }
        };
        match self.push(meta, buffer) {
            Ok(pushed) => pushed.into(),
            Err(PushError::Full(..)) => SubmitStatus::Full,
            Err(PushError::Closed(..)) => SubmitStatus::Closed,
        }
    }

    fn close(&self) {
        Self::close(self);
    }

    fn close_and_drain(&self) {
        Self::close_and_drain(self);
    }
}

/// Consumer end of a sink, from
/// [`Runtime::open_frame_sink`](crate::Runtime::open_frame_sink).
///
/// Hand [`id`](Self::id) to the producer (as a plugin method
/// argument), then receive. Dropping the sink closes it: producers are
/// told on their next push and every queued frame is released.
pub struct FrameSink<M> {
    shared: Arc<Shared<M>>,
    wake: Receiver<()>,
    runtime: Weak<Runtime>,
}

impl<M> FrameSink<M> {
    /// The id producers address this sink by.
    #[must_use]
    pub fn id(&self) -> FrameSinkId<M> {
        FrameSinkId::from_raw(self.shared.id)
    }

    /// The sink's policy.
    #[must_use]
    pub fn policy(&self) -> SinkPolicy {
        self.shared.policy
    }

    /// Lifetime counters.
    #[must_use]
    pub fn stats(&self) -> SinkStats {
        self.shared.stats()
    }

    /// Whether producers can no longer push. Queued frames may remain.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }

    /// The next queued frame, without waiting. `None` when nothing is
    /// queued (see [`is_closed`](Self::is_closed) to tell an ended sink
    /// apart).
    #[must_use]
    pub fn try_recv(&self) -> Option<NativeFrame<M>> {
        match self.shared.pop() {
            Pop::Frame(frame) => Some(frame),
            Pop::Empty | Pop::Ended => None,
        }
    }

    /// Block until a frame arrives; `None` once the sink is closed and
    /// drained.
    #[must_use]
    pub fn recv(&self) -> Option<NativeFrame<M>> {
        loop {
            match self.shared.pop() {
                Pop::Frame(frame) => return Some(frame),
                Pop::Ended => return None,
                Pop::Empty => {
                    // The doorbell sender lives in `shared`, which this
                    // sink keeps alive: the wait only ends on a token.
                    let _ = self.wake.recv();
                }
            }
        }
    }

    /// Wait for a frame on any executor; `None` once the sink is closed
    /// and drained.
    pub async fn recv_async(&self) -> Option<NativeFrame<M>> {
        loop {
            match self.shared.pop() {
                Pop::Frame(frame) => return Some(frame),
                Pop::Ended => return None,
                Pop::Empty => {
                    let _ = self.wake.recv_async().await;
                }
            }
        }
    }

    /// Close the sink now, keeping already queued frames receivable.
    pub fn close(&self) {
        self.shared.close();
    }
}

impl<M> Drop for FrameSink<M> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.forget_frame_sink(self.shared.id);
        }
        self.shared.close_and_drain();
    }
}

impl<M> fmt::Debug for FrameSink<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSink")
            .field("id", &self.shared.id)
            .field("policy", &self.shared.policy)
            .field("stats", &self.shared.stats())
            .finish_non_exhaustive()
    }
}

struct SenderGuard<M> {
    shared: Arc<Shared<M>>,
}

impl<M> Drop for SenderGuard<M> {
    fn drop(&mut self) {
        self.shared.close();
    }
}

/// Producer end of a sink, from
/// [`Runtime::frame_sender`](crate::Runtime::frame_sender).
///
/// Clones share one producer: the sink closes when the last clone drops
/// (or on [`close`](Self::close)). Resolve the id once per session and
/// clone, rather than resolving it again: each resolution is its own
/// producer and closes the sink when dropped.
pub struct FrameSender<M> {
    guard: Arc<SenderGuard<M>>,
}

impl<M> Clone for FrameSender<M> {
    fn clone(&self) -> Self {
        Self {
            guard: Arc::clone(&self.guard),
        }
    }
}

impl<M> FrameSender<M> {
    /// The sink this sender feeds.
    #[must_use]
    pub fn id(&self) -> FrameSinkId<M> {
        FrameSinkId::from_raw(self.guard.shared.id)
    }

    /// Hand a frame to the consumer. Never blocks.
    ///
    /// # Errors
    ///
    /// The frame comes back in [`PushError::Full`] when the sink refuses
    /// it ([`Overflow::DropNewest`]), or [`PushError::Closed`] when the
    /// consumer is gone — stop producing then.
    pub fn push(&self, meta: M, buffer: NativeBuffer) -> Result<Pushed, PushError<M>> {
        self.guard.shared.push(meta, buffer)
    }

    /// Whether pushes are refused for good.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.guard.shared.is_closed()
    }

    /// Lifetime counters of the sink.
    #[must_use]
    pub fn stats(&self) -> SinkStats {
        self.guard.shared.stats()
    }

    /// End the stream: the consumer receives what is queued, then
    /// `None`.
    pub fn close(&self) {
        self.guard.shared.close();
    }
}

impl<M> fmt::Debug for FrameSender<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSender")
            .field("id", &self.guard.shared.id)
            .finish_non_exhaustive()
    }
}

/// A new sink registered under `id`: the consumer end, and the shared
/// state for the runtime's registry.
pub(super) fn open<M: Message + Send + 'static>(
    id: u64,
    policy: SinkPolicy,
    runtime: Weak<Runtime>,
) -> (FrameSink<M>, Arc<Shared<M>>) {
    let (doorbell, wake) = flume::bounded(1);
    let shared = Arc::new(Shared {
        id,
        policy,
        state: Mutex::new(State {
            queue: VecDeque::with_capacity(policy.depth.get()),
            closed: false,
            next_seq: 0,
            pending_drops: 0,
            stats: SinkStats::default(),
        }),
        doorbell,
    });
    let sink = FrameSink {
        shared: Arc::clone(&shared),
        wake,
        runtime,
    };
    (sink, shared)
}

/// A new producer end of `shared`.
pub(super) fn sender<M>(shared: Arc<Shared<M>>) -> FrameSender<M> {
    FrameSender {
        guard: Arc::new(SenderGuard { shared }),
    }
}
