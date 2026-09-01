//! Type-aware wrapper over the raw byte-oriented [`StreamHandle`].
//!
//! Generated plugin glue returns `TypedStream<Item, Err>` so the plugin
//! author never touches raw bytes. `next` decodes both the per-event payload
//! (`Item`) and the terminal error variant (`Err`, if present) using the
//! wire codec.

use std::marker::PhantomData;

use crate::codec;
use crate::error::IstmoError;
use crate::message::Message;
use crate::protocol::{StreamEndReason, StreamId};
use crate::routing::StreamMessage;
use crate::runtime::StreamHandle;

/// One decoded observation on a stream.
#[derive(Debug, PartialEq, Eq)]
pub enum StreamItem<T, E> {
    /// A regular event.
    Event(T),
    /// The stream ended normally.
    Completed,
    /// The stream ended because it was cancelled from the Rust side.
    Cancelled,
    /// The stream ended with a domain error.
    Failed(E),
}

/// Typed wrapper over [`StreamHandle`]. `Item` is the per-event value, `Err`
/// is the domain error variant emitted by [`StreamEndReason::Error`].
#[derive(Debug)]
pub struct TypedStream<Item, Err = ()> {
    inner: StreamHandle,
    _marker: PhantomData<fn() -> (Item, Err)>,
}

impl<Item, Err> TypedStream<Item, Err>
where
    Item: Message,
    Err: Message,
{
    /// Wraps a raw stream handle.
    #[must_use]
    pub const fn new(inner: StreamHandle) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    /// The id of the underlying stream.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.inner.stream_id()
    }

    /// Blocks until the next observation arrives.
    pub fn recv(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        let msg = self.inner.recv()?;
        decode_message(msg)
    }

    /// Awaits the next observation.
    pub async fn recv_async(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        let msg = self.inner.recv_async().await?;
        decode_message(msg)
    }

    /// Non-blocking read. Returns `None` when there is nothing queued.
    pub fn try_recv(&self) -> Option<Result<StreamItem<Item, Err>, IstmoError>> {
        self.inner.try_recv().map(decode_message)
    }

    /// Unwraps back into the raw handle. Useful for tests and for glue
    /// that needs to observe raw bytes.
    #[must_use]
    pub fn into_inner(self) -> StreamHandle {
        self.inner
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
