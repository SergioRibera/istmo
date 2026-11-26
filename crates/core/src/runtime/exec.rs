//! Where hosted work runs.
//!
//! Native targets keep two sets of persistent threads, started on first
//! use and never one per call:
//!
//! - the async workers ([`spawn`], [`run_here`]): a few threads polling
//!   futures that must not block — [`DispatchMode::Inline`] hosts,
//!   stream pumps and encoders. [`run_here`] polls a future on the
//!   calling thread first, so one that completes without waiting never
//!   leaves it;
//! - the blocking workers ([`spawn_blocking`]): threads for
//!   [`DispatchMode::Isolated`] work, which may block or call back into
//!   the runtime and wait.
//!
//! `wasm32-unknown-unknown` has no threads: every future goes to the
//! browser's event loop through `wasm_bindgen_futures::spawn_local`, and
//! every wait inside it is an `.await`, never a blocking wait.
//!
//! [`DispatchMode::Inline`]: crate::DispatchMode::Inline
//! [`DispatchMode::Isolated`]: crate::DispatchMode::Isolated

use std::any::Any;
use std::cell::Cell;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::{Pin, pin};
use std::task::{Context, Poll};

use flume::{Sender, TrySendError};

use crate::dispatch::CancelToken;

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod blocking;
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod tasks;

/// Run `task` on the async workers. It must not block the thread
/// polling it.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(super) fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    tasks::spawn(task);
}

/// Run `task` on the browser's event loop.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(super) fn spawn(task: impl Future<Output = ()> + Send + 'static) {
    wasm_bindgen_futures::spawn_local(task);
}

/// Poll `task` on the calling thread until it first waits, then leave
/// it to the async workers. `true` when it finished before returning.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(super) fn run_here(task: impl Future<Output = ()> + Send + 'static) -> bool {
    // An inline host must not block whatever thread delivered it.
    let _delivering = DeliveryScope::enter();
    tasks::run_here(task)
}

mod delivery {
    // The initialiser is a `const` block already; clippy's
    // `missing_const_for_thread_local` still fires on some targets.
    #![allow(
        clippy::missing_const_for_thread_local,
        reason = "false positive on some targets; the initialiser is const"
    )]

    use std::cell::Cell;

    thread_local! {
        /// Open [`super::DeliveryScope`]s on this thread.
        pub(super) static DEPTH: Cell<u32> = const { Cell::new(0) };
    }
}

/// Marks the current thread as delivering work while it lives: the
/// runtime never blocks it waiting for room on the outbound channel.
///
/// A transport opens one for the life of a thread whose progress the
/// outbound channel depends on, such as its pump, so a send made from
/// there (by code the pump calls into) can never wait for the pump
/// itself. Frames that would have to wait are queued in order behind
/// the channel instead. The runtime opens one itself while it polls an
/// inline host on the thread that delivered the call, and while it
/// handles inbound frames.
///
/// Scopes nest; the thread is unmarked when the outermost one drops.
#[derive(Debug)]
#[must_use = "the thread is only marked while the scope lives"]
pub struct DeliveryScope {
    /// Thread-local state: the scope must drop on the thread that
    /// opened it.
    _not_send: std::marker::PhantomData<*const ()>,
}

impl DeliveryScope {
    /// Mark the current thread until the returned scope drops.
    pub fn enter() -> Self {
        delivery::DEPTH.with(|depth| depth.set(depth.get().saturating_add(1)));
        Self {
            _not_send: std::marker::PhantomData,
        }
    }

    /// The current thread is inside a scope.
    #[must_use]
    pub fn is_active() -> bool {
        delivery::DEPTH.with(Cell::get) > 0
    }
}

impl Drop for DeliveryScope {
    fn drop(&mut self) {
        delivery::DEPTH.with(|depth| depth.set(depth.get().saturating_sub(1)));
    }
}

/// Whether a synchronous send may block this thread on a full channel:
/// not on an async worker, not inside a [`DeliveryScope`].
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(super) fn may_block() -> bool {
    !tasks::on_worker() && !DeliveryScope::is_active()
}

/// On `wasm32` synchronous sends never wait in the first place (a full
/// channel fails them), so nothing changes there.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(super) const fn may_block() -> bool {
    true
}

/// Poll `task` once here; a pending one goes to the browser's event
/// loop.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub(super) fn run_here(task: impl Future<Output = ()> + Send + 'static) -> bool {
    let mut task = Box::pin(task);
    let mut cx = Context::from_waker(std::task::Waker::noop());
    // The noop waker loses the wake-up, so the event loop polls the task
    // afresh instead of relying on this poll's registration.
    match task.as_mut().poll(&mut cx) {
        Poll::Ready(()) => true,
        Poll::Pending => {
            wasm_bindgen_futures::spawn_local(task);
            false
        }
    }
}

/// Run `task` to completion on a blocking worker, which it may block.
///
/// # Errors
///
/// The OS refused a thread while every blocking worker was busy; `task`
/// is dropped unrun.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(super) fn spawn_blocking(
    task: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    blocking::spawn(move || pollster::block_on(task))
}

/// Run `task` on the browser's event loop: nothing can block there, so
/// the blocking workers are the event loop too.
///
/// # Errors
///
/// Never: the browser event loop always takes the task.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature as the native spawn_blocking"
)]
pub(super) fn spawn_blocking(
    task: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    wasm_bindgen_futures::spawn_local(task);
    Ok(())
}

/// Pending once (waking itself), then ready: hands the rest of a
/// [`run_here`] task to the workers.
pub(super) async fn yield_now() {
    let mut yielded = false;
    std::future::poll_fn(|cx| {
        if yielded {
            Poll::Ready(())
        } else {
            yielded = true;
            cx.waker().wake_by_ref();
            Poll::Pending
        }
    })
    .await;
}

/// Finish a send from a synchronous caller.
///
/// Native targets block until it completes. On `wasm32` nothing else
/// can run while this frame waits, so the send gets one try: a send
/// that would have to wait is dropped and logged.
pub(super) fn finish_now(send: impl Future<Output = ()>) {
    #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
    pollster::block_on(send);
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    {
        let mut send = pin!(send);
        let mut cx = Context::from_waker(std::task::Waker::noop());
        if send.as_mut().poll(&mut cx).is_pending() {
            tracing::warn!("a hosted response would have to wait on wasm32; dropped");
        }
    }
}

/// Which of two futures finished first.
pub(super) enum Either<A, B> {
    Left(A),
    Right(B),
}

/// Wait for whichever of `left` and `right` finishes first, dropping the
/// other. `left` is polled first, so it wins a tie.
pub(super) async fn race<A: Future, B: Future>(left: A, right: B) -> Either<A::Output, B::Output> {
    let mut left = pin!(left);
    let mut right = pin!(right);
    std::future::poll_fn(|cx| {
        if let Poll::Ready(value) = left.as_mut().poll(cx) {
            return Poll::Ready(Either::Left(value));
        }
        if let Poll::Ready(value) = right.as_mut().poll(cx) {
            return Poll::Ready(Either::Right(value));
        }
        Poll::Pending
    })
    .await
}

/// A future that turns a panic while polling `F` into an `Err`, like
/// [`catch_unwind`] does for a closure.
pub(super) struct CatchUnwind<F>(Pin<Box<F>>);

impl<F: Future> CatchUnwind<F> {
    pub(super) fn new(future: F) -> Self {
        Self(Box::pin(future))
    }
}

impl<F: Future> Future for CatchUnwind<F> {
    type Output = Result<F::Output, Box<dyn Any + Send>>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let inner = self.0.as_mut();
        // A host that panics may leave its own state half-updated either
        // way; catching the panic at least answers the call.
        match catch_unwind(AssertUnwindSafe(|| inner.poll(cx))) {
            Ok(Poll::Pending) => Poll::Pending,
            Ok(Poll::Ready(value)) => Poll::Ready(Ok(value)),
            Err(panic) => Poll::Ready(Err(panic)),
        }
    }
}

/// What became of a [`send_unless_cancelled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Delivery {
    Sent,
    Cancelled,
    Disconnected,
}

/// Send `message`, waiting while `tx` is full, unless `cancel` trips
/// first.
pub(super) async fn send_unless_cancelled<T>(
    tx: &Sender<T>,
    message: T,
    cancel: &CancelToken,
) -> Delivery {
    if cancel.is_cancelled() {
        return Delivery::Cancelled;
    }
    let message = match tx.try_send(message) {
        Ok(()) => return Delivery::Sent,
        Err(TrySendError::Disconnected(_)) => return Delivery::Disconnected,
        Err(TrySendError::Full(message)) => message,
    };
    match race(tx.send_async(message), cancel.cancelled()).await {
        Either::Left(Ok(())) => Delivery::Sent,
        Either::Left(Err(_)) => Delivery::Disconnected,
        Either::Right(()) => Delivery::Cancelled,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn race_takes_the_ready_side() {
        let left = pollster::block_on(race(async { 1 }, std::future::pending::<()>()));
        assert!(matches!(left, Either::Left(1)));
        let right = pollster::block_on(race(std::future::pending::<()>(), async { 2 }));
        assert!(matches!(right, Either::Right(2)));
    }

    #[test]
    fn catch_unwind_reports_panics() {
        assert_eq!(
            pollster::block_on(CatchUnwind::new(async { 3 })).ok(),
            Some(3)
        );
        let panicked = pollster::block_on(CatchUnwind::new(async {
            panic!("host bug");
        }));
        assert!(panicked.is_err());
    }

    #[test]
    fn send_waits_for_room_or_cancel() {
        let (tx, rx) = flume::bounded(1);
        let cancel = CancelToken::new();
        assert_eq!(
            pollster::block_on(send_unless_cancelled(&tx, 1, &cancel)),
            Delivery::Sent
        );
        // Full: the receiver drains from another thread.
        let drain = std::thread::spawn(move || (rx.recv().unwrap(), rx.recv().unwrap()));
        assert_eq!(
            pollster::block_on(send_unless_cancelled(&tx, 2, &cancel)),
            Delivery::Sent
        );
        assert_eq!(drain.join().unwrap(), (1, 2));

        let (tx, _rx) = flume::bounded(1);
        tx.send(0).unwrap();
        let cancel = CancelToken::new();
        let canceller = cancel.clone();
        let cancelling = std::thread::spawn(move || canceller.cancel());
        assert_eq!(
            pollster::block_on(send_unless_cancelled(&tx, 1, &cancel)),
            Delivery::Cancelled
        );
        cancelling.join().unwrap();

        let (tx, rx) = flume::bounded::<u8>(1);
        drop(rx);
        assert_eq!(
            pollster::block_on(send_unless_cancelled(&tx, 1, &CancelToken::new())),
            Delivery::Disconnected
        );
    }
}
