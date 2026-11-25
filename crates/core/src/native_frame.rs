//! Native frame sinks: high-rate buffers that never touch the wire.
//!
//! A [`FrameSink`] is a per-session mailbox owned by the consumer. Its
//! [`FrameSinkId`] travels to the producer as an ordinary plugin
//! argument; the producer pushes [`NativeFrame`]s — plugin-defined
//! metadata plus a [`NativeBuffer`] — that reach the consumer without
//! being encoded, routed or copied:
//!
//! - a Rust host in the same process resolves the id with
//!   [`Runtime::frame_sender`](crate::Runtime::frame_sender) and pushes
//!   values directly;
//! - Kotlin and Swift hosts hand platform buffers (`HardwareBuffer`,
//!   `CVPixelBuffer`, direct `ByteBuffer`s) to the transport crates'
//!   generic entry points, which wrap them and call
//!   [`Runtime::submit_native_frame`](crate::Runtime::submit_native_frame).
//!
//! The producer never blocks: a full sink drops frames according to its
//! [`SinkPolicy`] and counts every drop. Dropping a frame releases its
//! buffer back to whoever produced it.
//!
//! ```
//! use istmo_core::native_frame::{NativeBuffer, SinkPolicy};
//! use istmo_core::Runtime;
//!
//! let rt = Runtime::mock().runtime;
//! let sink = rt.open_frame_sink::<u32>(SinkPolicy::latest()).expect("running");
//! let sender = rt.frame_sender(sink.id()).expect("same runtime, same type");
//! sender.push(7, NativeBuffer::from_cpu(vec![0u8; 16])).expect("open sink");
//! let frame = sink.try_recv().expect("queued");
//! assert_eq!(frame.meta, 7);
//! assert_eq!(frame.buffer.cpu_bytes().map(<[u8]>::len), Some(16));
//! ```

use std::collections::HashMap;
use std::fmt;
use std::hash::{Hash, Hasher};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use wincode::{SchemaRead, SchemaWrite};

use crate::message::Message;
use crate::runtime::Runtime;
use crate::sync::lock;
use sink::{ErasedSink, Shared};

mod buffer;
mod sink;

pub use buffer::{BufferLease, CpuBuffer, MemoryKind, NativeBuffer};
#[cfg(unix)]
pub use buffer::{DmaBuf, DmaBufPlane};
pub use sink::{
    FrameSender, FrameSink, NativeFrame, Overflow, PushError, Pushed, SinkPolicy, SinkStats,
    SubmitStatus,
};

/// Process-local address of a [`FrameSink`] whose frames carry
/// metadata `M`.
///
/// On the wire it is the bare `u64` (the same layout as
/// [`NativeHandleId`](crate::NativeHandleId)); the type parameter only
/// exists on the Rust side, so a plugin method taking
/// `FrameSinkId<CaptureMeta>` states the metadata type its producer
/// must push. The id is meaningless outside the runtime that opened the
/// sink.
#[derive(SchemaWrite, SchemaRead)]
pub struct FrameSinkId<M> {
    raw: u64,
    #[wincode(skip)]
    meta: PhantomData<fn() -> M>,
}

impl<M> FrameSinkId<M> {
    /// The id with raw value `raw`. Transports use it to rebuild an id
    /// received from native code; everyone else gets ids from
    /// [`FrameSink::id`].
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self {
            raw,
            meta: PhantomData,
        }
    }

    /// The raw `u64` carried on the wire.
    #[must_use]
    pub const fn raw(self) -> u64 {
        self.raw
    }
}

// Manual impls: derives would demand `M: Clone` / `M: Debug` / ...,
// which the id does not need — it never holds an `M`.
impl<M> Clone for FrameSinkId<M> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<M> Copy for FrameSinkId<M> {}

impl<M> PartialEq for FrameSinkId<M> {
    fn eq(&self, other: &Self) -> bool {
        self.raw == other.raw
    }
}

impl<M> Eq for FrameSinkId<M> {}

impl<M> Hash for FrameSinkId<M> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.raw.hash(state);
    }
}

impl<M> fmt::Debug for FrameSinkId<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FrameSinkId({})", self.raw)
    }
}

impl<M> fmt::Display for FrameSinkId<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.raw)
    }
}

/// The runtime's table of open sinks, by raw id.
#[derive(Default)]
pub(crate) struct FrameSinks {
    sinks: Mutex<HashMap<u64, Arc<dyn ErasedSink>>>,
}

impl FrameSinks {
    pub(crate) fn open<M: Message + Send + 'static>(
        &self,
        id: u64,
        policy: SinkPolicy,
        runtime: Weak<Runtime>,
    ) -> FrameSink<M> {
        let (sink, shared) = sink::open(id, policy, runtime);
        let erased: Arc<dyn ErasedSink> = shared;
        lock(&self.sinks).insert(id, erased);
        sink
    }

    fn get(&self, id: u64) -> Option<Arc<dyn ErasedSink>> {
        lock(&self.sinks).get(&id).cloned()
    }

    pub(crate) fn sender<M: Message + Send + 'static>(
        &self,
        id: u64,
    ) -> Result<FrameSender<M>, SenderLookup> {
        let erased = self.get(id).ok_or(SenderLookup::Unknown)?;
        let shared = erased
            .into_any()
            .downcast::<Shared<M>>()
            .map_err(|_| SenderLookup::TypeMismatch)?;
        Ok(sink::sender(shared))
    }

    /// Push wire-encoded metadata and a buffer into sink `id`.
    pub(crate) fn submit(&self, id: u64, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus {
        self.get(id)
            .map_or(SubmitStatus::UnknownSink, |sink| sink.submit(meta, buffer))
    }

    /// Close sink `id` from the producer side; `false` when unknown.
    pub(crate) fn close(&self, id: u64) -> bool {
        self.get(id).is_some_and(|sink| {
            sink.close();
            true
        })
    }

    pub(crate) fn remove(&self, id: u64) {
        lock(&self.sinks).remove(&id);
    }

    /// Close every sink and forget them (runtime shutdown).
    pub(crate) fn close_all(&self) {
        let sinks: Vec<_> = lock(&self.sinks).drain().map(|(_, sink)| sink).collect();
        for sink in sinks {
            sink.close_and_drain();
        }
    }
}

impl fmt::Debug for FrameSinks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSinks")
            .field("open", &lock(&self.sinks).len())
            .finish()
    }
}

/// Why [`FrameSinks::sender`] found nothing usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SenderLookup {
    Unknown,
    TypeMismatch,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec;

    #[test]
    fn sink_ids_encode_as_a_bare_u64() {
        let id = FrameSinkId::<String>::from_raw(300);
        let bytes = codec::encode(&id).expect("encode");
        assert_eq!(bytes, codec::encode(&300u64).expect("encode u64"));
        let (back, used) = codec::decode::<FrameSinkId<String>>(&bytes).expect("decode");
        assert_eq!((back, used), (id, bytes.len()));
    }
}
