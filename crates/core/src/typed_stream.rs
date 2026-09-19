//! Typed adapter around [`StreamHandle`].
//!
//! [`TypedStream`] decodes each raw bincode payload into a strongly
//! typed [`StreamItem`], so consumers work with domain values instead of
//! byte slices.

use std::marker::PhantomData;

use crate::codec;
use crate::error::IstmoError;
use crate::message::Message;
use crate::protocol::{StreamEndReason, StreamId};
use crate::routing::StreamMessage;
use crate::runtime::StreamHandle;

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

/// Typed wrapper around a raw [`StreamHandle`].
///
/// Each item and terminal error are decoded from bincode bytes into the
/// caller-declared types. The three receive shapes ([`recv`](Self::recv),
/// [`recv_async`](Self::recv_async), [`try_recv`](Self::try_recv)) mirror
/// [`StreamHandle`]'s and behave identically otherwise.
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
    /// Wrap an existing [`StreamHandle`] in a typed adapter.
    ///
    /// The item type parameters must match the plugin's declared shape.
    #[must_use]
    pub const fn new(inner: StreamHandle) -> Self {
        Self {
            inner,
            _marker: PhantomData,
        }
    }

    /// [`StreamId`] of the wrapped handle.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.inner.stream_id()
    }

    /// Block the current thread for the next decoded [`StreamItem`].
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] on channel disconnect, or
    /// [`IstmoError::Codec`] if the payload cannot be decoded against
    /// `Item` / `Err`.
    pub fn recv(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        let msg = self.inner.recv()?;
        decode_message(msg)
    }

    /// Async variant of [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// Same as [`recv`](Self::recv).
    pub async fn recv_async(&self) -> Result<StreamItem<Item, Err>, IstmoError> {
        let msg = self.inner.recv_async().await?;
        decode_message(msg)
    }

    /// Non-blocking peek. Returns `None` if no item is buffered.
    pub fn try_recv(&self) -> Option<Result<StreamItem<Item, Err>, IstmoError>> {
        self.inner.try_recv().map(decode_message)
    }

    /// Unwrap the underlying raw [`StreamHandle`].
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
