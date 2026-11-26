//! Outbound frames that could not wait for room on the channel.
//!
//! A thread that delivers work (a transport's pump, a platform thread
//! running an inline host, an async worker) must never block on a full
//! outbound channel: the thread that would drain it may be waiting for
//! that very thread. So a send from there that finds the channel full is
//! queued here instead, and a task on the async workers moves the queue
//! to the channel in order, waiting for room as long as it takes.
//!
//! While anything is queued here every later frame queues behind it,
//! whoever sends it, so no frame overtakes an earlier one from the same
//! thread. Senders that may wait still wait: until their own frame left
//! this queue, so they keep the backpressure the channel gives them.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use flume::{Sender, TrySendError};

use super::exec::{self, Delivery, send_unless_cancelled};
use crate::dispatch::CancelToken;
use crate::error::IstmoError;
use crate::protocol::Envelope;
use crate::sync::lock;

/// What became of a queued frame, for a sender waiting on it.
pub(super) type Sent = Result<bool, IstmoError>;

pub(super) struct Spill {
    outbound: Sender<Envelope>,
    /// The runtime's outbound backlog: a queued frame keeps its count
    /// until it is sent, and gives it back if it never is.
    backlog: Arc<AtomicUsize>,
    /// Shutdown: queued frames stop waiting for room.
    stopped: CancelToken,
    /// Something is queued, or the drain task still runs.
    active: AtomicBool,
    queue: Mutex<Queue>,
}

#[derive(Default)]
struct Queue {
    frames: VecDeque<Queued>,
    next_ticket: u64,
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
        Self {
            outbound,
            backlog,
            stopped,
            active: AtomicBool::new(false),
            queue: Mutex::default(),
        }
    }

    /// Frames are queued here, so every new one must queue behind them.
    pub(super) fn is_active(&self) -> bool {
        self.active.load(Ordering::Acquire)
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
        let start = !self.active.swap(true, Ordering::AcqRel);
        drop(queue);
        if start {
            exec::spawn(Arc::clone(self).drain());
        }
        ticket.map(|done| Ticket { id, done })
    }

    /// Take a queued frame back before it went out, with its count;
    /// `false` when the drain task has it already.
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

    #[expect(
        clippy::significant_drop_tightening,
        reason = "the flag must clear under the lock a push takes"
    )]
    fn pop(&self) -> Option<Queued> {
        let mut queue = lock(&self.queue);
        let next = queue.frames.pop_front();
        if next.is_none() {
            // Under the lock: a push from now on starts a new drain.
            self.active.store(false, Ordering::Release);
        }
        next
    }

    async fn drain(self: Arc<Self>) {
        while let Some(Queued { envelope, done, .. }) = self.pop() {
            let sent = self.send(envelope).await;
            if !matches!(sent, Ok(true)) {
                tracing::debug!(?sent, "queued outbound frame undelivered");
                self.backlog.fetch_sub(1, Ordering::AcqRel);
            }
            // The sender may have stopped waiting: nobody to tell then.
            if let Some(done) = done {
                let _ = done.send(sent);
            }
        }
    }

    /// Like a blocking send: room first, shut down or not; then wait for
    /// room until shutdown.
    async fn send(&self, envelope: Envelope) -> Sent {
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
