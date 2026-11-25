//! Native frame sinks: high-rate buffers that never touch the wire.
//!
//! A [`FrameSink`] is a per-session mailbox owned by the consumer. Its
//! [`FrameSinkId`] travels to the producer as an ordinary plugin
//! argument; the producer pushes [`NativeFrame`]s — plugin-defined
//! metadata plus a [`NativeBuffer`] — that reach the consumer without
//! being encoded, routed or copied:
//!
//! - a Rust host in the same process resolves the id with
//!   [`HostCtx::frame_sender`](crate::HostCtx::frame_sender) (or
//!   [`Runtime::frame_sender`](crate::Runtime::frame_sender)) and pushes
//!   values directly;
//! - Kotlin and Swift hosts get an `IstmoFrameSender` from the generated
//!   dispatcher, which opens a [`NativeProducer`] once and hands it
//!   platform buffers (`HardwareBuffer`, `CVPixelBuffer`, pool slots).
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
//! sender.push(7, NativeBuffer::from_bytes(vec![0u8; 16])).expect("open sink");
//! let frame = sink.try_recv().expect("queued");
//! assert_eq!(frame.meta, 7);
//! assert_eq!(frame.buffer.as_bytes().map(<[u8]>::len), Some(16));
//! drop(sender);
//! assert!(sink.recv().is_none(), "the only producer left");
//! ```

use std::collections::hash_map::RandomState;
use std::collections::{HashMap, VecDeque};
use std::fmt;
use std::hash::{BuildHasher, Hash, Hasher};
use std::marker::PhantomData;
use std::sync::{Arc, Mutex, Weak};

use wincode::{SchemaRead, SchemaWrite};

use crate::message::Message;
use crate::runtime::Runtime;
use crate::sync::lock;
use sink::{ErasedSink, Shared};

mod buffer;
mod sink;

pub use buffer::{
    BufferIdentity, BufferLease, IntoBackingError, MemoryKind, NativeBuffer, PlatformBuffer,
};
#[cfg(unix)]
pub use buffer::{DmaBuf, DmaBufPlane};
pub use sink::{
    EndReason, FrameSender, FrameSink, NativeFrame, Overflow, PushError, Pushed, SinkError,
    SinkPolicy, SinkStats, SubmitStatus,
};

/// Version of the native entry points the transports export.
///
/// Android exports it as `nativeAbiVersion`, Apple as
/// `istmo_abi_version`. The Kotlin and Swift runtimes refuse to start against a library built
/// with another one.
pub const NATIVE_ABI_VERSION: u32 = 2;

/// Process-local address of a [`FrameSink`] whose frames carry
/// metadata `M`.
///
/// On the wire it is the bare `u64` (the same layout as
/// [`NativeHandleId`](crate::NativeHandleId)); the type parameter only
/// exists on the Rust side, so a plugin method taking
/// `FrameSinkId<CaptureMeta>` states the metadata type its producer
/// must push. Ids are random, so one cannot be guessed from another,
/// and meaningless outside the runtime that opened the sink: only
/// in-process producers can resolve one, and generated clients refuse
/// to send one to a plugin hosted in another process.
#[derive(SchemaWrite, SchemaRead)]
pub struct FrameSinkId<M> {
    raw: u64,
    #[wincode(skip)]
    meta: PhantomData<fn() -> M>,
}

impl<M> FrameSinkId<M> {
    /// The id with raw value `raw`. Transports and tests use it to
    /// rebuild an id; producers get ids as plugin arguments.
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
        write!(f, "FrameSinkId({:#018x})", self.raw)
    }
}

impl<M> fmt::Display for FrameSinkId<M> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:#018x}", self.raw)
    }
}

/// The fingerprint of metadata type `M` a native producer presents
/// when it opens a sink: FNV-1a 64 of the type's name with every module
/// path dropped (`Vec<CaptureMeta>`, `u32`, `String`).
///
/// `istmo-build` computes the same value from a plugin's contract for
/// the Kotlin and Swift senders it generates, so a producer generated
/// for another metadata type fails with
/// [`SubmitStatus::TypeMismatch`] instead of feeding bytes that happen
/// to decode.
#[must_use]
pub fn meta_fingerprint<M: ?Sized>() -> u64 {
    fingerprint_of(&short_type_name(std::any::type_name::<M>()))
}

/// FNV-1a 64 of `name`.
#[must_use]
pub const fn fingerprint_of(name: &str) -> u64 {
    let bytes = name.as_bytes();
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut i = 0;
    while i < bytes.len() {
        hash ^= bytes[i] as u64;
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        i += 1;
    }
    hash
}

/// `full` with every `a::b::` path prefix and every space dropped:
/// `alloc::vec::Vec<my_crate::Meta>` → `Vec<Meta>`.
fn short_type_name(full: &str) -> String {
    let mut out = String::with_capacity(full.len());
    let mut segment = String::new();
    let mut chars = full.chars().peekable();
    while let Some(c) = chars.next() {
        if c == ':' && chars.peek() == Some(&':') {
            chars.next();
            segment.clear();
        } else if c.is_alphanumeric() || c == '_' {
            segment.push(c);
        } else {
            out.push_str(&segment);
            segment.clear();
            if !c.is_whitespace() {
                out.push(c);
            }
        }
    }
    out.push_str(&segment);
    out
}

/// A producer handle a Kotlin or Swift host opened on a sink: resolved
/// once, then submitted to without touching the runtime's registry.
///
/// It counts as one producer of the sink for as long as it lives:
/// dropping it without [`close`](Self::close) ends the sink as
/// [`EndReason::ProducerGone`]. The transports hand it to native code as
/// an opaque pointer; the native `IstmoFrameSender` owns it and frees it
/// when closed.
pub struct NativeProducer {
    sink: Arc<dyn ErasedSink>,
}

impl NativeProducer {
    /// The raw id of the sink this producer feeds.
    #[must_use]
    pub fn sink_id(&self) -> u64 {
        self.sink.id()
    }

    /// Push a frame whose metadata is still wire-encoded. `meta` must
    /// decode as exactly the sink's metadata type. Whatever the status,
    /// `buffer` is either queued or released before this returns.
    #[must_use]
    pub fn submit(&self, meta: &[u8], buffer: NativeBuffer) -> SubmitStatus {
        self.sink.submit(meta, buffer)
    }

    /// Whether the sink stopped accepting frames.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.sink.is_closed()
    }

    /// End the stream as [`EndReason::Completed`].
    pub fn close(&self) {
        self.sink.end(EndReason::Completed, false);
    }

    /// End the stream as [`EndReason::Failed`] with `message`.
    pub fn close_with_message(&self, message: impl Into<String>) {
        self.sink
            .end(EndReason::Failed(SinkError::message(message)), false);
    }
}

impl Drop for NativeProducer {
    fn drop(&mut self) {
        self.sink.detach();
    }
}

impl fmt::Debug for NativeProducer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("NativeProducer")
            .field("sink", &self.sink.id())
            .field("meta", &self.sink.type_name())
            .finish_non_exhaustive()
    }
}

/// The runtime's table of open sinks, by raw id.
pub(crate) struct FrameSinks {
    sinks: Mutex<HashMap<u64, Arc<dyn ErasedSink>>>,
    /// Ids of the most recently closed sinks, oldest first, so a native
    /// producer that opens late hears `Closed` rather than
    /// `UnknownSink`. Bounded by [`RECENTLY_CLOSED`].
    closed: Mutex<VecDeque<u64>>,
    /// Keyed hash the ids are drawn from: random per runtime, so ids
    /// can neither be guessed nor collide across runtimes.
    ids: RandomState,
}

/// How many closed sink ids [`FrameSinks`] remembers.
pub(crate) const RECENTLY_CLOSED: usize = 256;

impl Default for FrameSinks {
    fn default() -> Self {
        Self {
            sinks: Mutex::new(HashMap::new()),
            closed: Mutex::new(VecDeque::new()),
            ids: RandomState::new(),
        }
    }
}

impl FrameSinks {
    /// Open a sink under a fresh id (`counter` makes each draw distinct).
    pub(crate) fn open<M: Message + Send + 'static>(
        &self,
        counter: u64,
        policy: SinkPolicy,
        runtime: Weak<Runtime>,
    ) -> FrameSink<M> {
        let mut sinks = lock(&self.sinks);
        let mut draw = counter;
        let id = loop {
            let id = self.ids.hash_one(draw);
            if id != 0 && !sinks.contains_key(&id) {
                break id;
            }
            draw = draw.wrapping_add(1 << 32);
        };
        let (sink, shared) = sink::open(id, policy, meta_fingerprint::<M>(), runtime);
        let erased: Arc<dyn ErasedSink> = shared;
        sinks.insert(id, erased);
        sink
    }

    fn get(&self, id: u64) -> Option<Arc<dyn ErasedSink>> {
        lock(&self.sinks).get(&id).cloned()
    }

    fn was_closed(&self, id: u64) -> bool {
        lock(&self.closed).contains(&id)
    }

    pub(crate) fn sender<M: Message + Send + 'static>(
        &self,
        id: u64,
    ) -> Result<FrameSender<M>, SenderLookup> {
        let erased = self.get(id).ok_or(SenderLookup::Unknown)?;
        let found = erased.type_name();
        let shared = erased
            .into_any()
            .downcast::<Shared<M>>()
            .map_err(|_| SenderLookup::TypeMismatch { found })?;
        Ok(sink::sender(shared))
    }

    /// A native producer on sink `id` presenting metadata `fingerprint`
    /// (0: unchecked).
    pub(crate) fn producer(
        &self,
        id: u64,
        fingerprint: u64,
    ) -> Result<NativeProducer, SubmitStatus> {
        let Some(sink) = self.get(id) else {
            return Err(if self.was_closed(id) {
                SubmitStatus::Closed
            } else {
                SubmitStatus::UnknownSink
            });
        };
        if fingerprint != 0 && fingerprint != sink.fingerprint() {
            tracing::warn!(
                sink = id,
                meta = sink.type_name(),
                "a native producer for another metadata type tried to open this sink"
            );
            return Err(SubmitStatus::TypeMismatch);
        }
        sink.attach();
        Ok(NativeProducer { sink })
    }

    fn remember_closed(&self, ids: impl IntoIterator<Item = u64>) {
        let mut closed = lock(&self.closed);
        for id in ids {
            if closed.len() == RECENTLY_CLOSED {
                closed.pop_front();
            }
            closed.push_back(id);
        }
    }

    /// Forget sink `id` (its consumer left), remembering it as closed.
    pub(crate) fn remove(&self, id: u64) {
        lock(&self.sinks).remove(&id);
        self.remember_closed([id]);
    }

    /// End every sink and forget them (runtime shutdown).
    pub(crate) fn close_all(&self) {
        let sinks: Vec<_> = lock(&self.sinks).drain().map(|(_, sink)| sink).collect();
        self.remember_closed(sinks.iter().map(|sink| sink.id()));
        for sink in sinks {
            sink.end(EndReason::Shutdown, true);
        }
    }
}

impl fmt::Debug for FrameSinks {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FrameSinks")
            .field("open", &lock(&self.sinks).len())
            .field("recently_closed", &lock(&self.closed).len())
            .finish_non_exhaustive()
    }
}

/// Why [`FrameSinks::sender`] found nothing usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SenderLookup {
    Unknown,
    TypeMismatch { found: &'static str },
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

    #[test]
    fn short_type_names_drop_paths() {
        assert_eq!(short_type_name("u32"), "u32");
        assert_eq!(short_type_name("alloc::string::String"), "String");
        assert_eq!(
            short_type_name("alloc::vec::Vec<core::option::Option<my_crate::api::Meta>>"),
            "Vec<Option<Meta>>"
        );
        assert_eq!(short_type_name("(u32, my::X)"), "(u32,X)");
        // What `type_name` really prints today: a rustc that changes its
        // format fails here before native producers see a mismatch.
        assert_eq!(short_type_name(std::any::type_name::<Vec<u8>>()), "Vec<u8>");
        assert_eq!(
            short_type_name(std::any::type_name::<Option<FrameSinks>>()),
            "Option<FrameSinks>"
        );
        assert_eq!(meta_fingerprint::<Vec<u8>>(), fingerprint_of("Vec<u8>"));
        assert_ne!(meta_fingerprint::<u32>(), meta_fingerprint::<u64>());
    }
}
