//! Outbound frames that could not wait for room on the channel.
//!
//! A thread that must never block (a transport's pump, a process
//! bridge's reader, anything inside a [`DeliveryScope`]; a synchronous
//! send on an async worker) may find the outbound channel full: the
//! thread that drains it may be waiting for that very thread. Its frame
//! is queued here instead, and a drain thread moves the queue to the
//! channel in order, waiting for room as long as it takes. The drain is
//! a thread of its own, not a task on the async workers, so a host that
//! blocks every worker cannot hold back a plain thread waiting behind
//! the queue. It stays [`LINGER`] after the queue empties, so a
//! transport running close to full does not pay a thread start per
//! burst.
//!
//! While anything is queued here every later frame queues behind it,
//! whoever sends it, so no frame overtakes an earlier one from the same
//! thread. Senders that may wait still wait: until their own frame left
//! this queue, so they keep the backpressure the channel gives them.
//!
//! The queue has no bound: the threads feeding it must not wait, and
//! dropping frames would lose answers. Past [`HIGH_WATER`] channels'
//! worth of frames it logs a warning (at most every [`WARN_EVERY`]), and
//! [`SpillStats`] counts what it holds.
//!
//! At shutdown, frames queued without a waiting sender (whose send
//! already returned) get [`SHUTDOWN_FLUSH`] more to reach a transport
//! that keeps draining; whatever is left then is dropped and the drain
//! exits, so it never holds the channel open after shutdown. A sender
//! still waiting on its frame takes it back and fails, as any blocking
//! send does at shutdown.
//!
//! [`DeliveryScope`]: super::DeliveryScope

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
use flume::SendTimeoutError;
use flume::{Sender, TrySendError};

use super::exec::{self, Delivery, send_unless_cancelled};
use crate::dispatch::CancelToken;
use crate::error::IstmoError;
use crate::protocol::Envelope;
use crate::sync::lock;

/// How long the drain thread waits for more frames once the queue is
/// empty, before it exits.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
const LINGER: Duration = Duration::from_secs(1);

/// The drain thread waits for room in slices, starting here and doubling
/// up to [`MAX_SLICE`], to notice a shutdown.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
const FIRST_SLICE: Duration = Duration::from_millis(5);

/// The longest slice of a wait for room.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
const MAX_SLICE: Duration = Duration::from_millis(250);

/// How long queued frames may still wait for room after shutdown.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
const SHUTDOWN_FLUSH: Duration = Duration::from_millis(500);

/// Queued frames, in channel capacities, from which the queue warns.
const HIGH_WATER: usize = 4;

/// Least time between two high-water warnings.
const WARN_EVERY: Duration = Duration::from_secs(10);

/// What a runtime queued behind its full outbound channel; see
/// [`Runtime::outbound_spill`](super::Runtime::outbound_spill).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct SpillStats {
    /// Frames queued now.
    pub queued: usize,
    /// Most frames ever queued at once.
    pub peak: usize,
    /// Frames ever queued.
    pub total: u64,
}

/// What became of a queued frame, for a sender waiting on it.
pub(super) type Sent = Result<bool, IstmoError>;

pub(super) struct Spill {
    outbound: Sender<Envelope>,
    /// The runtime's outbound backlog: a queued frame keeps its count
    /// until it is sent, and gives it back if it never is.
    backlog: Arc<AtomicUsize>,
    /// Shutdown: queued frames stop waiting for room.
    stopped: CancelToken,
    /// Something is queued, or the drain is sending a frame it took.
    active: AtomicBool,
    queue: Mutex<Queue>,
    /// Wakes a lingering drain thread: a frame was queued, or shutdown.
    wake: Condvar,
    /// Queue length from which [`push`](Self::push) warns.
    high_water: usize,
    peak: AtomicUsize,
    total: AtomicU64,
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<Queued>,
    next_ticket: u64,
    /// A drain runs, or lingers for more frames.
    draining: bool,
    last_warning: Option<Instant>,
    /// Shut down: until when queued frames may still wait for room.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    flush_until: Option<Instant>,
}

struct Queued {
    ticket: u64,
    envelope: Envelope,
    done: Option<oneshot::Sender<Sent>>,
}

/// A queued frame a sender waits on.
pub(super) struct Ticket {
    id: u64,
    pub(super) done: oneshot::Receiver<Sent>,
}

impl Spill {
    pub(super) fn new(
        outbound: Sender<Envelope>,
        backlog: Arc<AtomicUsize>,
        stopped: CancelToken,
    ) -> Self {
        let capacity = outbound
            .capacity()
            .unwrap_or(super::DEFAULT_OUTBOUND_CAPACITY);
        Self {
            outbound,
            backlog,
            stopped,
            active: AtomicBool::new(false),
            queue: Mutex::default(),
            wake: Condvar::new(),
            high_water: HIGH_WATER.saturating_mul(capacity.max(1)),
            peak: AtomicUsize::new(0),
            total: AtomicU64::new(0),
        }
    }

    /// Frames are queued here, so every new one must queue behind them.
    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
    }

    pub(super) fn stats(&self) -> SpillStats {
        SpillStats {
            queued: lock(&self.queue).frames.len(),
            peak: self.peak.load(Ordering::Relaxed),
            total: self.total.load(Ordering::Relaxed),
        }
    }

    /// Queue `envelope` behind the others; with `wait`, the sender gets a
    /// ticket to learn when it left. The caller already counted it in
    /// the backlog.
    pub(super) fn push(self: &Arc<Self>, envelope: Envelope, wait: bool) -> Option<Ticket> {
        let (done_tx, ticket) = if wait {
            let (tx, rx) = oneshot::channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let mut queue = lock(&self.queue);
        let id = queue.next_ticket;
        queue.next_ticket += 1;
        queue.frames.push_back(Queued {
            ticket: id,
            envelope,
            done: done_tx,
        });
        self.active.store(true, Ordering::Release);
        let queued = queue.frames.len();
        let start = !std::mem::replace(&mut queue.draining, true);
        let warn = queued >= self.high_water
            && queue
                .last_warning
                .is_none_or(|at| at.elapsed() >= WARN_EVERY);
        if warn {
            queue.last_warning = Some(Instant::now());
        }
        drop(queue);
        self.total.fetch_add(1, Ordering::Relaxed);
        self.peak.fetch_max(queued, Ordering::Relaxed);
        if warn {
            tracing::warn!(
                queued,
                high_water = self.high_water,
                "outbound frames pile up behind the full channel: a thread that must not wait \
                 sends faster than the transport drains"
            );
        }
        if start {
            self.start_drain();
        } else {
            self.wake.notify_one();
        }
        ticket.map(|done| Ticket { id, done })
    }

    /// Take a queued frame back before it went out, with its count;
    /// `false` when the drain has it already.
    pub(super) fn withdraw(&self, ticket: &Ticket) -> bool {
        let mut queue = lock(&self.queue);
        let Some(at) = queue.frames.iter().position(|q| q.ticket == ticket.id) else {
            return false;
        };
        queue.frames.remove(at);
        drop(queue);
        self.backlog.fetch_sub(1, Ordering::AcqRel);
        true
    }

    /// The runtime shut down (its token is cancelled): start the flush
    /// window, and wake a lingering drain so it exits now instead of
    /// holding the channel open.
    pub(super) fn stop(&self) {
        // Under the lock: a drain about to wait has either seen the
        // shutdown or is waiting already.
        #[cfg_attr(
            all(target_arch = "wasm32", target_os = "unknown"),
            allow(unused_mut, reason = "the flush window needs the drain thread")
        )]
        let mut queue = lock(&self.queue);
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        queue
            .flush_until
            .get_or_insert_with(|| Instant::now() + SHUTDOWN_FLUSH);
        self.wake.notify_all();
        drop(queue);
    }

    /// Shut down: until when queued frames may still wait for room.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn flush_deadline(&self) -> Option<Instant> {
        if !self.stopped.is_cancelled() {
            return None;
        }
        Some(
            *lock(&self.queue)
                .flush_until
                .get_or_insert_with(|| Instant::now() + SHUTDOWN_FLUSH),
        )
    }

    fn start_drain(self: &Arc<Self>) {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            let spill = Arc::clone(self);
            let started = std::thread::Builder::new()
                .name("istmo-spill".to_owned())
                .spawn(move || spill.drain());
            match started {
                Ok(_) => return,
                Err(err) => tracing::warn!(
                    %err,
                    "no thread for the outbound queue; draining it on the async workers"
                ),
            }
        }
        exec::spawn(Arc::clone(self).drain_async());
    }

    /// The next frame to send, waiting up to `linger` for one once the
    /// queue is empty; `None` ends the drain.
    fn next(&self, linger: Duration) -> Option<Queued> {
        let mut queue = lock(&self.queue);
        let mut idle_since = None;
        loop {
            if let Some(next) = queue.frames.pop_front() {
                return Some(next);
            }
            // Under the lock, and only once the frame taken before was
            // sent: from now on a push queues behind nothing.
            self.active.store(false, Ordering::Release);
            let left = linger.saturating_sub(idle_since.get_or_insert_with(Instant::now).elapsed());
            if left.is_zero() || self.stopped.is_cancelled() {
                queue.draining = false;
                return None;
            }
            queue = self
                .wake
                .wait_timeout(queue, left)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
    }

    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn drain(&self) {
        while let Some(Queued { envelope, done, .. }) = self.next(LINGER) {
            let sent = self.send_waiting(envelope);
            self.settle(sent, done);
        }
    }

    /// The drain without a thread of its own: `wasm32`, or the OS refused
    /// one.
    async fn drain_async(self: Arc<Self>) {
        while let Some(Queued { envelope, done, .. }) = self.next(Duration::ZERO) {
            let sent = self.send_async(envelope).await;
            self.settle(sent, done);
        }
    }

    fn settle(&self, sent: Sent, done: Option<oneshot::Sender<Sent>>) {
        if !matches!(sent, Ok(true)) {
            tracing::debug!(?sent, "queued outbound frame undelivered");
            self.backlog.fetch_sub(1, Ordering::AcqRel);
        }
        // The sender may have stopped waiting: nobody to tell then.
        if let Some(done) = done {
            let _ = done.send(sent);
        }
    }

    /// Like a blocking send: room first, shut down or not; then wait for
    /// room, after shutdown only until the flush window closes.
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    fn send_waiting(&self, envelope: Envelope) -> Sent {
        let mut envelope = match self.outbound.try_send(envelope) {
            Ok(()) => return Ok(true),
            Err(TrySendError::Disconnected(_)) => return Err(IstmoError::ChannelClosed),
            Err(TrySendError::Full(envelope)) => envelope,
        };
        let mut slice = FIRST_SLICE;
        loop {
            let now = Instant::now();
            let until = match self.flush_deadline() {
                Some(closed) if closed <= now => return Err(IstmoError::RuntimeShutDown),
                Some(closes) => closes.min(now + slice),
                None => now + slice,
            };
            match self.outbound.send_deadline(envelope, until) {
                Ok(()) => return Ok(true),
                Err(SendTimeoutError::Disconnected(_)) => return Err(IstmoError::ChannelClosed),
                Err(SendTimeoutError::Timeout(back)) => envelope = back,
            }
            slice = (slice * 2).min(MAX_SLICE);
        }
    }

    /// [`send_waiting`](Self::send_waiting) for an async drain.
    async fn send_async(&self, envelope: Envelope) -> Sent {
        let envelope = match self.outbound.try_send(envelope) {
            Ok(()) => return Ok(true),
            Err(TrySendError::Disconnected(_)) => return Err(IstmoError::ChannelClosed),
            Err(TrySendError::Full(envelope)) => envelope,
        };
        match send_unless_cancelled(&self.outbound, envelope, &self.stopped).await {
            Delivery::Sent => Ok(true),
            Delivery::Cancelled => Err(IstmoError::RuntimeShutDown),
            Delivery::Disconnected => Err(IstmoError::ChannelClosed),
        }
    }
}
