//! Typed stream consumer.
//!
//! [`TypedStream`] yields strongly typed [`StreamItem`]s, so consumers
//! work with domain values instead of byte slices. It is fed one of two
//! ways, invisible to the consumer:
//!
//! - **wire**: a [`StreamHandle`] whose payloads are decoded item by
//!   item (a native host, a remote process, or a Rust host registered
//!   without its typed shortcut);
//! - **local**: the producer's own `flume::Receiver`, handed over by a
//!   Rust host's typed in-process shortcut. Items are moved, never
//!   encoded, and no channel or thread sits between the producer and
//!   the consumer.

use std::fmt;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use flume::{Receiver, RecvError, TryRecvError};

use crate::codec;
use crate::dispatch::CancelToken;
use crate::error::IstmoError;
use crate::message::Message;
use crate::protocol::{StreamEndReason, StreamId};
use crate::routing::StreamMessage;
use crate::runtime::{Runtime, StreamHandle, recv_unless_cancelled};

/// One decoded event delivered by a [`TypedStream`].
#[derive(Debug, PartialEq, Eq)]
pub enum StreamItem<T, E> {
    /// A live event carrying a decoded value.
    Event(T),
    /// The producer signalled clean end-of-stream.
    Completed,
    /// The consumer or a wrapping cancellation signal cancelled the
    /// stream.
    Cancelled,
    /// The producer errored out. The decoded domain error is attached.
    Failed(E),
}

/// Typed consumer end of a plugin stream.
///
/// The three receive shapes ([`recv`](Self::recv),
/// [`recv_async`](Self::recv_async), [`try_recv`](Self::try_recv))
/// behave the same whichever way the stream is fed. Once a terminal
/// item (`Completed`, `Cancelled`, `Failed`) was handed out, further
/// receives fail with [`IstmoError::ChannelClosed`].
pub struct TypedStream<Item, Err = ()> {
    inner: Inner<Item, Err>,
}

enum Inner<Item, Err> {
    Wire(StreamHandle),
    Local(LocalStream<Item, Err>),
}

impl<Item, Err> fmt::Debug for TypedStream<Item, Err> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let path = match self.inner {
            Inner::Wire(_) => "wire",
            Inner::Local(_) => "local",
        };
        f.debug_struct("TypedStream")
            .field("stream_id", &self.stream_id())
            .field("path", &path)
            .finish_non_exhaustive()
    }
}

impl<Item, Err> TypedStream<Item, Err> {
    /// [`StreamId`] of the stream. Local streams get one from the same
    /// counter as wire streams.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        match &self.inner {
            Inner::Wire(handle) => handle.stream_id(),
            Inner::Local(local) => local.stream_id,
        }
    }

    /// Whether items are moved from an in-process producer rather than
    /// decoded from the wire.
    #[must_use]
    pub const fn is_local(&self) -> bool {
        matches!(self.inner, Inner::Local(_))
    }

    /// The underlying wire handle; `None` for a local stream, which
    /// carries no bytes.
    #[must_use]
    pub fn into_wire(self) -> Option<StreamHandle> {
        match self.inner {
            Inner::Wire(handle) => Some(handle),
            Inner::Local(_) => None,
        }
    }
}

impl<Item, Err> TypedStream<Item, Err>
where
    Item: Message,
    Err: Message,
{
    /// Wrap an existing [`StreamHandle`] in a typed adapter.
    ///
    /// The item type parameters must match the plugin's declared shape.
    #[must_use]
    pub const fn new(inner: StreamHandle) -> Self {
        Self {
            inner: Inner::Wire(inner),
        }
    }

    /// Open a local stream on `runtime` over the receiver `open`
    /// returns. Used by the typed shortcut `#[istmo::plugin]` generates.
    ///
    /// `open` gets the stream's [`CancelToken`]: it trips when the
    /// consumer drops the stream or the runtime shuts down. A panic in
    /// `open` is answered like a panicking host on the wire path.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::RuntimeShutDown`] after shutdown.
    /// - [`IstmoError::PluginError`] carrying `"host panicked"` when
    ///   `open` panicked.
    pub fn open_local(
        runtime: &Arc<Runtime>,
        open: impl FnOnce(CancelToken) -> Receiver<Item>,
    ) -> Result<Self, IstmoError> {
        LocalStream::open(runtime, open, LocalSource::Plain).map(Self::local)
    }

    /// [`open_local`](Self::open_local) for a fallible stream: the first
    /// `Err` ends it as [`StreamItem::Failed`].
    ///
    /// # Errors
    ///
    /// As [`open_local`](Self::open_local).
    pub fn open_local_fallible(
        runtime: &Arc<Runtime>,
        open: impl FnOnce(CancelToken) -> Receiver<Result<Item, Err>>,
    ) -> Result<Self, IstmoError> {
        LocalStream::open(runtime, open, LocalSource::Fallible).map(Self::local)
    }

    const fn local(local: LocalStream<Item, Err>) -> Self {
        Self {
            inner: Inner::Local(local),
        }
    }

    /// Block the current thread for the next [`StreamItem`].
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] once the stream ended or its
    /// channel is gone, or [`IstmoError::Codec`] if a wire payload
    /// cannot be decoded against `Item` / `Err`.
    pub fn recv(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        match &self.inner {
            Inner::Wire(handle) => decode_message(handle.recv()?),
            Inner::Local(local) => local.recv(),
        }
    }

    /// Async variant of [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// Same as [`recv`](Self::recv).
    pub async fn recv_async(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        match &self.inner {
            Inner::Wire(handle) => decode_message(handle.recv_async().await?),
            Inner::Local(local) => local.recv_async().await,
        }
    }

    /// Non-blocking peek. Returns `None` if no item is buffered.
    pub fn try_recv(&self) -> Option<Result<StreamItem<Item, Err>, IstmoError>> {
        match &self.inner {
            Inner::Wire(handle) => handle.try_recv().map(decode_message),
            Inner::Local(local) => local.try_step(),
        }
    }
}

/// The producer's receiver, as the trait method returned it.
enum LocalSource<Item, Err> {
    Plain(Receiver<Item>),
    Fallible(Receiver<Result<Item, Err>>),
}

/// One step of a local stream: an item, or nothing yet.
enum Step<Item, Err> {
    Item(StreamItem<Item, Err>),
    Empty,
}

struct LocalStream<Item, Err> {
    source: LocalSource<Item, Err>,
    stream_id: StreamId,
    /// Tripped on drop and at runtime shutdown.
    cancel: CancelToken,
    runtime: Weak<Runtime>,
    /// A terminal item was handed out.
    ended: AtomicBool,
}

impl<Item, Err> LocalStream<Item, Err> {
    fn open<R>(
        runtime: &Arc<Runtime>,
        open: impl FnOnce(CancelToken) -> Receiver<R>,
        wrap: impl FnOnce(Receiver<R>) -> LocalSource<Item, Err>,
    ) -> Result<Self, IstmoError> {
        let (stream_id, cancel) = runtime.begin_local_stream()?;
        let source = if let Ok(receiver) = catch_unwind(AssertUnwindSafe(|| open(cancel.clone()))) {
            wrap(receiver)
        } else {
            tracing::error!(?stream_id, "typed-shortcut stream host panicked");
            runtime.finish_local_stream(stream_id);
            let bytes = codec::encode(&"host panicked".to_owned()).unwrap_or_default();
            return Err(IstmoError::PluginError { bytes });
        };
        Ok(Self {
            source,
            stream_id,
            cancel,
            runtime: Arc::downgrade(runtime),
            ended: AtomicBool::new(false),
        })
    }

    fn end(&self, item: StreamItem<Item, Err>) -> StreamItem<Item, Err> {
        self.ended.store(true, Ordering::Relaxed);
        item
    }

    /// What the producer's item means for the consumer.
    fn item(&self, item: Result<Item, Err>) -> StreamItem<Item, Err> {
        match item {
            Ok(value) => StreamItem::Event(value),
            Err(err) => self.end(StreamItem::Failed(err)),
        }
    }

    /// What a finished wait means for the consumer.
    fn received(
        &self,
        received: Option<Result<Result<Item, Err>, RecvError>>,
    ) -> StreamItem<Item, Err> {
        match received {
            Some(Ok(item)) => self.item(item),
            Some(Err(RecvError::Disconnected)) => self.end(StreamItem::Completed),
            None => self.end(StreamItem::Cancelled),
        }
    }

    /// The next step without waiting.
    fn step(&self) -> Result<Step<Item, Err>, IstmoError> {
        if self.ended.load(Ordering::Relaxed) {
            return Err(IstmoError::ChannelClosed);
        }
        // A producer that never stops sending keeps its receiver ready,
        // so the flag is what guarantees a cancel is seen.
        if self.cancel.is_cancelled() {
            return Ok(Step::Item(self.end(StreamItem::Cancelled)));
        }
        let received = match &self.source {
            LocalSource::Plain(rx) => rx.try_recv().map(Ok),
            LocalSource::Fallible(rx) => rx.try_recv(),
        };
        Ok(match received {
            Ok(item) => Step::Item(self.item(item)),
            Err(TryRecvError::Empty) => Step::Empty,
            Err(TryRecvError::Disconnected) => Step::Item(self.end(StreamItem::Completed)),
        })
    }

    fn try_step(&self) -> Option<Result<StreamItem<Item, Err>, IstmoError>> {
        match self.step() {
            Ok(Step::Item(item)) => Some(Ok(item)),
            Ok(Step::Empty) => None,
            Err(err) => Some(Err(err)),
        }
    }

    fn recv(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        if let Step::Item(item) = self.step()? {
            return Ok(item);
        }
        Ok(pollster::block_on(self.wait()))
    }

    async fn recv_async(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        if let Step::Item(item) = self.step()? {
            return Ok(item);
        }
        Ok(self.wait().await)
    }

    /// Wait for the producer's next item or the cancel, whichever comes
    /// first.
    async fn wait(&self) -> StreamItem<Item, Err> {
        let received = match &self.source {
            LocalSource::Plain(rx) => recv_unless_cancelled(rx, &self.cancel)
                .await
                .map(|item| item.map(Ok)),
            LocalSource::Fallible(rx) => recv_unless_cancelled(rx, &self.cancel).await,
        };
        self.received(received)
    }
}

impl<Item, Err> Drop for LocalStream<Item, Err> {
    fn drop(&mut self) {
        // Wakes a producer waiting on its token; one that only sends
        // notices when its `send` fails on the dropped receiver.
        self.cancel.cancel();
        if let Some(runtime) = self.runtime.upgrade() {
            runtime.finish_local_stream(self.stream_id);
        }
    }
}

fn decode_message<Item, Err>(msg: StreamMessage) -> Result<StreamItem<Item, Err>, IstmoError>
where
    Item: Message,
    Err: Message,
{
    match msg {
        StreamMessage::Event(bytes) => {
            let (value, _) = codec::decode::<Item>(&bytes)?;
            Ok(StreamItem::Event(value))
        }
        StreamMessage::End(StreamEndReason::Complete) => Ok(StreamItem::Completed),
        StreamMessage::End(StreamEndReason::Cancelled) => Ok(StreamItem::Cancelled),
        StreamMessage::End(StreamEndReason::Error(bytes)) => {
            let (err, _) = codec::decode::<Err>(&bytes)?;
            Ok(StreamItem::Failed(err))
        }
    }
}
