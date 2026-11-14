//! Where hosted work runs.
//!
//! Every hosted call, instance creation, notification and stream pump
//! is a future handed to [`spawn`]:
//!
//! - native targets run it on its own named OS thread under
//!   [`pollster::block_on`], so a host may block or call back into the
//!   runtime without stalling anything else;
//! - `wasm32-unknown-unknown` has no threads: the future goes to the
//!   browser's event loop through `wasm_bindgen_futures::spawn_local`,
//!   and every wait inside it is an `.await`, never a blocking wait.

use std::any::Any;
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::{Pin, pin};
use std::task::{Context, Poll};

use flume::{Sender, TrySendError};

use crate::dispatch::CancelToken;

/// Run `task` in the background.
///
/// # Errors
///
/// The OS refused a thread (native targets only).
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub(super) fn spawn(
    name: &str,
    task: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(move || pollster::block_on(task))
        .map(drop)
}

/// Run `task` in the background.
///
/// # Errors
///
/// Never: the browser event loop always takes the task.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[expect(
    clippy::unnecessary_wraps,
    reason = "same signature as the native spawn"
)]
pub(super) fn spawn(
    _name: &str,
    task: impl Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    wasm_bindgen_futures::spawn_local(task);
    Ok(())
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
