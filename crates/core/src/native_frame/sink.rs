//! [`FrameSink`] / [`FrameSender`]: the bounded, never-blocking
//! mailbox between producers and one consumer.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::num::NonZeroUsize;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::task::{Context, Poll, Wake, Waker};

use super::{FrameSinkId, NativeBuffer};
use crate::codec;
use crate::message::Message;
use crate::runtime::Runtime;
use crate::sync::lock;

/// What a full sink does with one more frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
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
    ///
    /// # Panics
    ///
    /// When `depth` is 0: a sink that can hold nothing would drop every
    /// frame.
    #[must_use]
    pub const fn queue(depth: usize) -> Self {
        let Some(depth) = NonZeroUsize::new(depth) else {
            panic!("SinkPolicy::queue needs a depth of at least 1");
        };
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
    /// counting from 0. Gaps are frames evicted by the policy.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }

    /// Frames the sink evicted since the previous delivered frame.
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
#[non_exhaustive]
pub struct SinkStats {
    /// Frames the sink took in.
    pub accepted: u64,
    /// Frames handed to the consumer.
    pub delivered: u64,
    /// Accepted frames lost to [`Overflow::DropOldest`]: evicted before
    /// the consumer took them.
    pub evicted: u64,
    /// Frames [`Overflow::DropNewest`] refused and handed back to the
    /// producer. A producer that retries a refused frame counts it once
    /// per attempt; it is not lost unless the producer gives up.
    pub refused: u64,
    /// Accepted frames released undelivered when the consumer dropped
    /// the sink or the runtime shut down.
    pub discarded: u64,
}

/// Why a sink stopped accepting frames.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum EndReason {
    /// A producer ended the stream ([`FrameSender::close`]).
    Completed,
    /// Every producer handle dropped without closing the stream (a
    /// crashed or forgotten producer).
    ProducerGone,
    /// A producer ended the stream with an error
    /// ([`FrameSender::close_with`]).
    Failed(SinkError),
    /// The consumer closed it ([`FrameSink::close`]) or dropped it.
    ConsumerClosed,
    /// The runtime shut down.
    Shutdown,
}

impl EndReason {
    /// Whether the producer finished on purpose.
    #[must_use]
    pub const fn is_completed(&self) -> bool {
        matches!(self, Self::Completed)
    }

    /// The producer's error, for [`Failed`](Self::Failed).
    #[must_use]
    pub const fn error(&self) -> Option<&SinkError> {
        match self {
            Self::Failed(err) => Some(err),
            Self::Completed | Self::ProducerGone | Self::ConsumerClosed | Self::Shutdown => None,
        }
    }
}

impl fmt::Display for EndReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Completed => f.write_str("the producer completed the stream"),
            Self::ProducerGone => f.write_str("every producer went away without closing"),
            Self::Failed(err) => write!(f, "the producer failed: {err}"),
            Self::ConsumerClosed => f.write_str("the consumer closed the sink"),
            Self::Shutdown => f.write_str("the runtime shut down"),
        }
    }
}

/// The error a producer ended a sink with; cheap to clone.
#[derive(Clone)]
pub struct SinkError(Arc<dyn std::error::Error + Send + Sync>);

impl SinkError {
    /// Wrap `error`.
    #[must_use]
    pub fn new(error: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self(Arc::new(error))
    }

    /// An error that only carries `message` (what native producers
    /// send).
    #[must_use]
    pub fn message(message: impl Into<String>) -> Self {
        Self::new(MessageError(message.into()))
    }

    /// The wrapped error.
    #[must_use]
    pub fn get(&self) -> &(dyn std::error::Error + Send + Sync + 'static) {
        &*self.0
    }
}

impl fmt::Debug for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&*self.0, f)
    }
}

impl fmt::Display for SinkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(&*self.0, f)
    }
}

impl std::error::Error for SinkError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&*self.0)
    }
}

#[derive(Debug)]
struct MessageError(String);

impl fmt::Display for MessageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for MessageError {}

/// What [`FrameSender::push`] did with a frame it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pushed {
    /// Queued; nothing was lost.
    Accepted,
    /// Queued after evicting the oldest queued frame.
    Displaced,
}

/// A frame the sink did not take, given back to the producer.
#[non_exhaustive]
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

/// Outcome of a native submit, as the integer the transports return to
/// Kotlin, Swift and C.
///
/// For [`Accepted`](Self::Accepted) and [`Displaced`](Self::Displaced)
/// the sink holds the frame. [`Internal`](Self::Internal) means Rust
/// failed after taking over the buffer: it is (or will be) released by
/// Rust, the caller must not release it again. For every other status
/// Rust kept nothing: what it was handed is already released.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(i32)]
#[non_exhaustive]
pub enum SubmitStatus {
    /// Queued.
    Accepted = 0,
    /// Queued after evicting an older frame.
    Displaced = 1,
    /// The sink ended (consumer gone, a producer closed it, shutdown):
    /// stop producing.
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
    /// A malformed argument (null object, negative length, a freed
    /// producer).
    Invalid = 7,
    /// The sink is full and refuses new frames.
    Full = 8,
    /// Rust failed internally after taking over the buffer (a bug);
    /// Rust releases what it took.
    Internal = 9,
    /// The sink carries another metadata type than the producer's.
    TypeMismatch = 10,
}

impl SubmitStatus {
    /// Every status, in code order.
    pub const ALL: [Self; 11] = [
        Self::Accepted,
        Self::Displaced,
        Self::Closed,
        Self::UnknownSink,
        Self::BadMeta,
        Self::Unsupported,
        Self::NotStarted,
        Self::Invalid,
        Self::Full,
        Self::Internal,
        Self::TypeMismatch,
    ];

    /// The integer native callers see.
    #[must_use]
    pub const fn code(self) -> i32 {
        self as i32
    }

    /// The status for `code`; `None` for a code this build does not know.
    #[must_use]
    pub const fn from_code(code: i32) -> Option<Self> {
        let mut i = 0;
        while i < Self::ALL.len() {
            if Self::ALL[i].code() == code {
                return Some(Self::ALL[i]);
            }
            i += 1;
        }
        None
    }

    /// The `SCREAMING_SNAKE` name the Kotlin enum and C constants use.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Accepted => "ACCEPTED",
            Self::Displaced => "DISPLACED",
            Self::Closed => "CLOSED",
            Self::UnknownSink => "UNKNOWN_SINK",
            Self::BadMeta => "BAD_META",
            Self::Unsupported => "UNSUPPORTED",
            Self::NotStarted => "NOT_STARTED",
            Self::Invalid => "INVALID",
            Self::Full => "FULL",
            Self::Internal => "INTERNAL",
            Self::TypeMismatch => "TYPE_MISMATCH",
        }
    }

    /// Whether the sink kept the frame.
    #[must_use]
    pub const fn is_taken(self) -> bool {
        matches!(self, Self::Accepted | Self::Displaced)
    }

    /// Whether every later submit will fail the same way: the producer
    /// should stop.
    #[must_use]
    pub const fn should_stop(self) -> bool {
        matches!(
            self,
            Self::Closed | Self::UnknownSink | Self::NotStarted | Self::TypeMismatch
        )
    }
}

impl From<Pushed> for SubmitStatus {
    fn from(pushed: Pushed) -> Self {
        match pushed {
            Pushed::Accepted => Self::Accepted,
            Pushed::Displaced => Self::Displaced,
        }
    }
}

/// Wakers of the receivers waiting for a frame. Every push wakes all of
/// them; each one looks again. One consumer (the usual case) never
/// allocates.
#[derive(Default)]
struct Waiters {
    first: Option<Waker>,
    more: Vec<Waker>,
}

impl Waiters {
    fn register(&mut self, waker: &Waker) {
        let known = |w: &Waker| w.will_wake(waker);
        if self.first.as_ref().is_some_and(known) || self.more.iter().any(known) {
            return;
        }
        if self.first.is_none() {
            self.first = Some(waker.clone());
        } else {
            self.more.push(waker.clone());
        }
    }

    /// The registered wakers, leaving none.
    fn take(&mut self) -> Option<Self> {
        self.first.as_ref()?;
        Some(std::mem::take(self))
    }

    fn wake(self) {
        if let Some(first) = self.first {
            first.wake();
        }
        for waker in self.more {
            waker.wake();
        }
    }
}

struct State<M> {
    queue: VecDeque<NativeFrame<M>>,
    /// Set once: no frame is accepted any more; queued ones are still
    /// delivered.
    end: Option<EndReason>,
    waiters: Waiters,
    next_seq: u64,
    pending_evictions: u64,
    stats: SinkStats,
}

/// The sink itself, shared by the consumer end, the producer ends and
/// the runtime's registry.
pub(super) struct Shared<M> {
    id: u64,
    policy: SinkPolicy,
    fingerprint: u64,
    state: Mutex<State<M>>,
    /// Live producer handles: Rust senders (every clone) and native
    /// producers. The last one to go ends the sink as
    /// [`EndReason::ProducerGone`].
    producers: AtomicUsize,
    /// A malformed native submit was logged already.
    warned: AtomicBool,
}

/// Drop `value`, logging instead of unwinding when its drop panics: a
/// release that panics must not take the producer or consumer with it.
pub(super) fn drop_quietly<T>(value: T, what: &'static str) {
    if catch_unwind(AssertUnwindSafe(move || drop(value))).is_err() {
        tracing::error!(what, "dropping a frame panicked; continuing");
    }
}

impl<M> Shared<M> {
    fn push(&self, meta: M, buffer: NativeBuffer) -> Result<Pushed, PushError<M>> {
        let evicted;
        let wake;
        {
            let mut state = lock(&self.state);
            if state.end.is_some() {
                return Err(PushError::Closed(meta, buffer));
            }
            evicted = if state.queue.len() < self.policy.depth.get() {
                None
            } else {
                match self.policy.overflow {
                    Overflow::DropOldest => {
                        state.pending_evictions += 1;
                        state.stats.evicted += 1;
                        state.queue.pop_front()
                    }
                    Overflow::DropNewest => {
                        state.stats.refused += 1;
                        return Err(PushError::Full(meta, buffer));
                    }
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
            wake = state.waiters.take();
        }
        if let Some(wake) = wake {
            wake.wake();
        }
        // Released outside the lock: a release may call into JNI or
        // CoreFoundation, or push into this very sink.
        Ok(evicted.map_or(Pushed::Accepted, |frame| {
            drop_quietly(frame, "evicted frame");
            Pushed::Displaced
        }))
    }

    /// The next frame, or the end; registers `waker` when there is
    /// neither.
    fn poll_pop(&self, waker: Option<&Waker>) -> Poll<Option<NativeFrame<M>>> {
        let mut state = lock(&self.state);
        if let Some(mut frame) = state.queue.pop_front() {
            frame.dropped_before = std::mem::take(&mut state.pending_evictions);
            state.stats.delivered += 1;
            return Poll::Ready(Some(frame));
        }
        if state.end.is_some() {
            return Poll::Ready(None);
        }
        if let Some(waker) = waker {
            state.waiters.register(waker);
        }
        Poll::Pending
    }

    /// End the sink with `reason` unless it ended already; with `drain`,
    /// also release every queued frame. Wakes every waiting receiver.
    fn end(&self, reason: EndReason, drain: bool) {
        let (queued, wake) = {
            let mut state = lock(&self.state);
            if state.end.is_none() {
                state.end = Some(reason);
            }
            let queued = if drain {
                let queued = std::mem::take(&mut state.queue);
                state.stats.discarded += queued.len() as u64;
                queued
            } else {
                VecDeque::new()
            };
            (queued, state.waiters.take())
        };
        if let Some(wake) = wake {
            wake.wake();
        }
        drop_quietly(queued, "queued frames");
    }

    fn end_reason(&self) -> Option<EndReason> {
        lock(&self.state).end.clone()
    }

    fn is_closed(&self) -> bool {
        lock(&self.state).end.is_some()
    }

    fn stats(&self) -> SinkStats {
        lock(&self.state).stats
    }

    fn attach(&self) {
        self.producers.fetch_add(1, Ordering::Relaxed);
    }

    fn detach(&self) {
        if self.producers.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.end(EndReason::ProducerGone, false);
        }
    }
}

/// Type-erased view of a sink, for the registry and native producers.
pub(super) trait ErasedSink: Send + Sync {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync>;
    fn id(&self) -> u64;
    fn fingerprint(&self) -> u64;
    fn type_name(&self) -> &'static str;
    fn is_closed(&self) -> bool;
    fn submit(&self, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus;
    fn attach(&self);
    fn detach(&self);
    fn end(&self, reason: EndReason, drain: bool);
}

impl<M: Message + Send + 'static> ErasedSink for Shared<M> {
    fn into_any(self: Arc<Self>) -> Arc<dyn Any + Send + Sync> {
        self
    }

    fn id(&self) -> u64 {
        self.id
    }

    fn fingerprint(&self) -> u64 {
        self.fingerprint
    }

    fn type_name(&self) -> &'static str {
        std::any::type_name::<M>()
    }

    fn is_closed(&self) -> bool {
        Self::is_closed(self)
    }

    fn submit(&self, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus {
        let meta = match codec::decode_exact::<M>(meta) {
            Ok(meta) => meta,
            Err(err) => {
                if !self.warned.swap(true, Ordering::Relaxed) {
                    tracing::warn!(
                        %err,
                        sink = self.id,
                        meta = std::any::type_name::<M>(),
                        "native frame metadata does not decode (reported once per sink)"
                    );
                }
                drop_quietly(buffer, "refused buffer");
                return SubmitStatus::BadMeta;
            }
        };
        match self.push(meta, buffer) {
            Ok(pushed) => pushed.into(),
            Err(refused) => {
                let status = if refused.is_closed() {
                    SubmitStatus::Closed
                } else {
                    SubmitStatus::Full
                };
                drop_quietly(refused, "refused frame");
                status
            }
        }
    }

    fn attach(&self) {
        Self::attach(self);
    }

    fn detach(&self) {
        Self::detach(self);
    }

    fn end(&self, reason: EndReason, drain: bool) {
        Self::end(self, reason, drain);
    }
}

/// Consumer end of a sink, from
/// [`Runtime::open_frame_sink`](crate::Runtime::open_frame_sink).
///
/// Hand [`id`](Self::id) to the producer (as a plugin method
/// argument), then receive with [`recv`](Self::recv),
/// [`recv_async`](Self::recv_async) or as a [`Stream`] of frames; once
/// it yields `None`, [`end_reason`](Self::end_reason) says why. Any
/// number of receivers may wait at once: every one of them wakes on
/// each frame and at the end.
///
/// Dropping the sink closes it: producers are told on their next push
/// and every queued frame is released.
///
/// [`Stream`]: futures_core::Stream
pub struct FrameSink<M> {
    shared: Arc<Shared<M>>,
    runtime: Weak<Runtime>,
}

/// Wakes the thread blocked in [`FrameSink::recv`].
struct ThreadWaker(std::thread::Thread);

impl Wake for ThreadWaker {
    fn wake(self: Arc<Self>) {
        self.0.unpark();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.0.unpark();
    }
}

thread_local! {
    /// This thread's waker, built once so blocking receives neither
    /// allocate nor register twice.
    static THREAD_WAKER: Waker = Waker::from(Arc::new(ThreadWaker(std::thread::current())));
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

    /// Why the sink ended; `None` while it is open.
    #[must_use]
    pub fn end_reason(&self) -> Option<EndReason> {
        self.shared.end_reason()
    }

    /// The next queued frame, without waiting. `None` when nothing is
    /// queued (see [`is_closed`](Self::is_closed) to tell an ended sink
    /// apart).
    #[must_use]
    pub fn try_recv(&self) -> Option<NativeFrame<M>> {
        match self.shared.poll_pop(None) {
            Poll::Ready(frame) => frame,
            Poll::Pending => None,
        }
    }

    /// Block until a frame arrives; `None` once the sink ended and is
    /// drained.
    #[must_use]
    pub fn recv(&self) -> Option<NativeFrame<M>> {
        static WAIT: crate::spin::SpinWait = crate::spin::SpinWait::new();
        THREAD_WAKER.with(|waker| {
            loop {
                if let Poll::Ready(frame) = self.shared.poll_pop(Some(waker)) {
                    return frame;
                }
                // A producer about to push is caught without a wake-up.
                if let Some(frame) = WAIT.spin(|| match self.shared.poll_pop(None) {
                    Poll::Ready(frame) => Some(frame),
                    Poll::Pending => None,
                }) {
                    return frame;
                }
                // A wake between the registration and here leaves the
                // park token set: `park` returns at once.
                std::thread::park();
            }
        })
    }

    /// Wait for a frame on any executor; `None` once the sink ended and
    /// is drained.
    pub async fn recv_async(&self) -> Option<NativeFrame<M>> {
        poll_fn(|cx| self.shared.poll_pop(Some(cx.waker()))).await
    }

    /// Close the sink now, keeping already queued frames receivable.
    pub fn close(&self) {
        self.shared.end(EndReason::ConsumerClosed, false);
    }
}

impl<M> futures_core::Stream for FrameSink<M> {
    type Item = NativeFrame<M>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.shared.poll_pop(Some(cx.waker()))
    }
}

impl<M> Drop for FrameSink<M> {
    fn drop(&mut self) {
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.forget_frame_sink(self.shared.id);
        }
        self.shared.end(EndReason::ConsumerClosed, true);
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

/// Producer end of a sink, from
/// [`Runtime::frame_sender`](crate::Runtime::frame_sender) or
/// [`HostCtx::frame_sender`](crate::HostCtx::frame_sender).
///
/// Every sender (each resolution and each clone) counts as one
/// producer: the sink ends as [`EndReason::ProducerGone`] when the last
/// one drops, so resolving the id again (a check, a second producer)
/// never closes a live stream. End it on purpose with
/// [`close`](Self::close) or [`close_with`](Self::close_with).
pub struct FrameSender<M> {
    shared: Arc<Shared<M>>,
}

impl<M> Clone for FrameSender<M> {
    fn clone(&self) -> Self {
        self.shared.attach();
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<M> Drop for FrameSender<M> {
    fn drop(&mut self) {
        self.shared.detach();
    }
}

impl<M> FrameSender<M> {
    /// The sink this sender feeds.
    #[must_use]
    pub fn id(&self) -> FrameSinkId<M> {
        FrameSinkId::from_raw(self.shared.id)
    }

    /// The sink's policy: how many frames it queues, so a producer can
    /// size its pool to keep frames flowing.
    #[must_use]
    pub fn policy(&self) -> SinkPolicy {
        self.shared.policy
    }

    /// Hand a frame to the consumer. Never blocks.
    ///
    /// # Errors
    ///
    /// The frame comes back in [`PushError::Full`] when the sink refuses
    /// it ([`Overflow::DropNewest`]), or [`PushError::Closed`] when the
    /// sink ended — stop producing then.
    pub fn push(&self, meta: M, buffer: NativeBuffer) -> Result<Pushed, PushError<M>> {
        self.shared.push(meta, buffer)
    }

    /// Whether pushes are refused for good.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.shared.is_closed()
    }

    /// Lifetime counters of the sink.
    #[must_use]
    pub fn stats(&self) -> SinkStats {
        self.shared.stats()
    }

    /// End the stream as [`EndReason::Completed`]: the consumer receives
    /// what is queued, then the end.
    pub fn close(&self) {
        self.shared.end(EndReason::Completed, false);
    }

    /// End the stream as [`EndReason::Failed`] with `error`.
    pub fn close_with(&self, error: impl std::error::Error + Send + Sync + 'static) {
        self.shared
            .end(EndReason::Failed(SinkError::new(error)), false);
    }
}

impl<M> fmt::Debug for FrameSender<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSender")
            .field("id", &self.shared.id)
            .finish_non_exhaustive()
    }
}

/// A new sink registered under `id`: the consumer end, and the shared
/// state for the runtime's registry.
pub(super) fn open<M: Message + Send + 'static>(
    id: u64,
    policy: SinkPolicy,
    fingerprint: u64,
    runtime: Weak<Runtime>,
) -> (FrameSink<M>, Arc<Shared<M>>) {
    let shared = Arc::new(Shared {
        id,
        policy,
        fingerprint,
        state: Mutex::new(State {
            queue: VecDeque::with_capacity(policy.depth.get()),
            end: None,
            waiters: Waiters::default(),
            next_seq: 0,
            pending_evictions: 0,
            stats: SinkStats::default(),
        }),
        producers: AtomicUsize::new(0),
        warned: AtomicBool::new(false),
    });
    let sink = FrameSink {
        shared: Arc::clone(&shared),
        runtime,
    };
    (sink, shared)
}

/// A new producer end of `shared`.
pub(super) fn sender<M>(shared: Arc<Shared<M>>) -> FrameSender<M> {
    shared.attach();
    FrameSender { shared }
}
