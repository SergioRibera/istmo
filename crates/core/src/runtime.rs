use std::any::Any;
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, TrySendError, bounded};

use crate::dispatch::{
    CallOrigin, CancelToken, Dispatch, DispatchError, DispatchMode, HostedStreamItem, Outcome,
    Plugin,
};
use crate::early_events::EarlyEventStore;
use crate::error::IstmoError;
use crate::main_thread::{InlineMainThread, MainThread};
use crate::message::Message;
use crate::native_frame::{
    FrameSender, FrameSink, FrameSinkId, FrameSinks, NativeProducer, SenderLookup, SinkPolicy,
    SubmitStatus,
};
use crate::protocol::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId, PROTOCOL_VERSION,
    StreamEndReason, StreamId,
};
use crate::routing::{CallResult, RoutingTables, StreamMessage};
use crate::sync::lock;

mod exec;
mod peer;
mod spill;

pub use exec::{BlockingThreads, DeliveryScope};
pub use peer::{DirectOutcome, DirectPeer};
pub use spill::SpillStats;

use exec::{CatchUnwind, Delivery, Either, race, send_unless_cancelled};

/// Default depth of the outbound envelope channel.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 256;

/// How many cancels for not-yet-dispatched hosted calls are remembered.
///
/// A [`Frame::Cancel`] can overtake the [`Frame::Call`] it targets, so a
/// cancel for an unknown id is kept until that call shows up. Cancels
/// for calls that already finished look the same and never get claimed,
/// so the set is a bounded FIFO: the oldest unclaimed cancel is
/// forgotten first.
const EARLY_CANCEL_CAPACITY: usize = 1024;

/// Startup parameters for [`Runtime::init`].
#[derive(Debug)]
pub struct RuntimeConfig {
    /// Adapter used to hop work onto the platform's main thread.
    pub main_thread: Arc<dyn MainThread>,
    /// Capacity of the outbound envelope channel — controls how many
    /// frames can queue up before a producer starts blocking.
    pub outbound_capacity: usize,
}

impl RuntimeConfig {
    /// A minimal configuration that runs every main-thread task inline
    /// on the caller.
    ///
    /// Suitable for desktop binaries and tests; mobile transports supply
    /// their own [`MainThread`] impl instead.
    #[must_use]
    pub fn inline() -> Self {
        Self {
            main_thread: Arc::new(InlineMainThread),
            outbound_capacity: DEFAULT_OUTBOUND_CAPACITY,
        }
    }
}

/// Result of [`Runtime::init`]: the shared [`Runtime`] plus the outbound
/// [`Envelope`] receiver the platform transport must drain.
#[derive(Debug)]
pub struct RuntimeInit {
    /// The freshly built runtime.
    pub runtime: Arc<Runtime>,
    /// Receiver for outbound envelopes. The platform transport (JNI
    /// pump on Android, FFI callback on iOS, or an in-process bridge)
    /// pulls from this channel and forwards each envelope to the peer.
    pub outbound: FlumeReceiver<Envelope>,
}

impl RuntimeInit {
    /// Register a Rust-hosted plugin dispatcher.
    ///
    /// Chainable with [`expects`](Self::expects) / [`remotes`](Self::remotes)
    /// / [`finish`](Self::finish); typically driven by the
    /// [`istmo::runtime!`](../../istmo_macros/macro.runtime.html) macro.
    #[must_use]
    pub fn host<D>(self, dispatcher: D) -> Self
    where
        D: Dispatch,
    {
        self.runtime.register_host(dispatcher);
        self
    }

    /// [`host`](Self::host) without the typed in-process shortcut, every
    /// call on a blocking worker: see
    /// [`Runtime::register_host_wire_only`]. `runtime!` uses it for
    /// entries marked `#[wire_only]`.
    #[must_use]
    pub fn host_wire_only<D>(self, dispatcher: D) -> Self
    where
        D: Dispatch,
    {
        self.runtime.register_host_wire_only(dispatcher);
        self
    }

    /// Declare that this runtime intends to call plugin `T`.
    ///
    /// Used by [`finish`](Self::finish) to enforce that every plugin the
    /// app touches is wired up before the runtime starts serving calls.
    #[must_use]
    pub fn expects<T>(self) -> Self
    where
        T: Plugin,
    {
        self.runtime.declare_plugin(T::PLUGIN_ID);
        self
    }

    /// Declare that plugin `T` lives in a remote process and its frames
    /// should be shuttled through the registered remote envelope sink.
    ///
    /// See [`Runtime::install_remote_envelope_sink`].
    #[must_use]
    pub fn remotes<T>(self) -> Self
    where
        T: Plugin,
    {
        self.runtime.declare_remote_plugin(T::PLUGIN_ID);
        self
    }

    /// Enable strict declaration enforcement.
    ///
    /// After this call, every outbound call goes through
    /// [`Runtime::check_declared`]; requests for plugins that were not
    /// wired up return [`IstmoError::PluginNotDeclared`].
    #[must_use]
    pub fn finish(self) -> Self {
        self.runtime.set_enforce_declarations(true);
        self
    }
}

/// Sink for outbound envelopes destined for a plugin hosted in another
/// process.
///
/// Installed via [`Runtime::install_remote_envelope_sink`]; the sink
/// receives the fully-encoded bytes and is expected to hand them to the
/// remote runtime through an IPC channel (Binder on Android, XPC on
/// Apple platforms, or anything else the app author wires up).
pub type RemoteEnvelopeSink = Arc<dyn Fn(Vec<u8>) + Send + Sync>;

/// Callback fired locally for every [`Frame::ReleaseNativeHandle`]
/// emitted by [`Runtime::release_native_handle`].
///
/// Installed via [`Runtime::install_native_handle_release_hook`]; hooks
/// are additive (multiple can be registered) and fire *in addition to*
/// the wire-side release frame that is sent to the outbound channel.
/// Rust-side hosts use this to free their own resources without
/// depending on an external transport pump to route the release back.
pub type NativeHandleReleaseHook = Arc<dyn Fn(NativeHandleId) + Send + Sync>;

/// Who issued a call served by a host registered on this runtime.
///
/// Decides where the response, events and stream end go: straight into
/// this runtime's routing tables, out to the peer, or through the remote
/// sink. Each issuer allocates call ids on its own, so ids from two
/// origins can coincide; tables keyed by call id for hosted work also
/// carry the origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum Origin {
    /// This runtime's own [`Runtime::call`] / [`Runtime::stream`] /
    /// [`Runtime::create_instance`].
    Local,
    /// A [`Frame::Call`] / [`Frame::CreateInstance`] from the peer
    /// (the platform transport), through [`Runtime::dispatch_inbound`].
    Peer,
    /// A [`Frame::Call`] / [`Frame::CreateInstance`] from a runtime in
    /// another process, through [`Runtime::inject_wire_envelope`].
    Remote,
}

impl From<Origin> for CallOrigin {
    fn from(origin: Origin) -> Self {
        match origin {
            Origin::Local => Self::Local,
            Origin::Peer => Self::Peer,
            Origin::Remote => Self::Remote,
        }
    }
}

/// One call on its way to a host registered on this runtime.
#[derive(Debug)]
struct HostedCall {
    origin: Origin,
    call_id: CallId,
    instance_id: Option<InstanceId>,
    method: String,
    payload: Vec<u8>,
}

/// Where a hosted call's answer goes while its synchronous caller
/// ([`Runtime::serve_peer_call`]) has not returned yet.
#[derive(Debug, Default)]
struct DirectReply {
    slot: Mutex<DirectSlot>,
}

#[derive(Debug, Default)]
enum DirectSlot {
    /// The caller still waits for the answer.
    #[default]
    Waiting,
    /// Answered before the caller returned; it takes the answer.
    Answered(CallResult),
    /// The caller returned without an answer: it goes out as a frame.
    Gone,
}

impl DirectReply {
    /// Leave `result` for the caller; handed back when the caller is
    /// gone and the answer must be sent instead.
    fn offer(&self, result: CallResult) -> Result<(), CallResult> {
        let mut slot = lock(&self.slot);
        if !matches!(*slot, DirectSlot::Waiting) {
            return Err(result);
        }
        *slot = DirectSlot::Answered(result);
        drop(slot);
        Ok(())
    }

    /// The caller returns: take the answer if there is one, otherwise
    /// let a later answer go out as a frame.
    fn close(&self) -> Option<CallResult> {
        let previous = std::mem::replace(&mut *lock(&self.slot), DirectSlot::Gone);
        match previous {
            DirectSlot::Answered(result) => Some(result),
            DirectSlot::Waiting | DirectSlot::Gone => None,
        }
    }
}

/// Credit a peer granted to one of its streams: the pump takes `rx`
/// when the stream opens, grants arrive on `tx`.
#[derive(Debug)]
struct CreditLane {
    tx: FlumeSender<u32>,
    rx: Option<FlumeReceiver<u32>>,
}

/// Cancellation state for calls served by hosts on this runtime.
#[derive(Debug, Default)]
struct HostedCalls {
    /// Hosted calls whose dispatch is running, and hosted streams whose
    /// pump is running.
    active: HashMap<(Origin, CallId), CancelToken>,
    /// Cancels for ids not in `active`; see [`EARLY_CANCEL_CAPACITY`].
    early_cancels: HashSet<(Origin, CallId)>,
    /// Insertion order of `early_cancels`, oldest first.
    early_order: VecDeque<(Origin, CallId)>,
    /// Set by [`cancel_all`](Self::cancel_all): no call starts any more.
    closed: bool,
}

impl HostedCalls {
    /// Start tracking a hosted call and hand back its token, already
    /// cancelled when the issuer's cancel overtook the call. `None` once
    /// the runtime shut down.
    fn begin(&mut self, origin: Origin, call_id: CallId) -> Option<CancelToken> {
        if self.closed {
            return None;
        }
        let token = CancelToken::new();
        if self.early_cancels.remove(&(origin, call_id)) {
            token.cancel();
        }
        self.active.insert((origin, call_id), token.clone());
        Some(token)
    }

    /// Cancel a hosted call or stream from `origin`.
    ///
    /// An active call is cancelled at once. Anything else is remembered
    /// until its call begins, in a FIFO of at most
    /// [`EARLY_CANCEL_CAPACITY`] ids. A running call is always in
    /// `active`, so eviction only ever forgets a cancel whose call has
    /// not arrived yet (or already finished).
    fn cancel(&mut self, origin: Origin, call_id: CallId) {
        if let Some(token) = self.active.get(&(origin, call_id)) {
            token.cancel();
            return;
        }
        // Local calls register before their handle exists, so only a
        // peer or remote cancel can arrive ahead of its call.
        let key = (origin, call_id);
        if origin == Origin::Local || self.closed || !self.early_cancels.insert(key) {
            return;
        }
        if self.early_order.len() == EARLY_CANCEL_CAPACITY
            && let Some(oldest) = self.early_order.pop_front()
        {
            self.early_cancels.remove(&oldest);
        }
        self.early_order.push_back(key);
    }

    fn finish(&mut self, origin: Origin, call_id: CallId) {
        self.active.remove(&(origin, call_id));
    }

    fn is_active(&self, origin: Origin, call_id: CallId) -> bool {
        self.active.contains_key(&(origin, call_id))
    }

    fn cancel_all(&mut self) {
        self.closed = true;
        for token in self.active.values() {
            token.cancel();
        }
        self.active.clear();
        self.early_cancels.clear();
        self.early_order.clear();
    }
}

/// What an outgoing call to a remote-process plugin was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OutgoingKind {
    /// [`Frame::Call`], unary or stream.
    Call,
    /// [`Frame::CreateInstance`]: the instance id in its response is
    /// recorded as remote.
    CreateInstance,
}

/// Calls crossing the remote-process sink, so their follow-up frames
/// take the same route. Every entry leaves on the call's terminal frame.
///
/// Two maps because the ids come from two allocators: `outgoing` holds
/// ids this runtime allocated, `incoming` ids the remote process did.
#[derive(Debug, Default)]
struct RemoteCalls {
    /// Calls this runtime sent to a remote plugin. Leave on the inbound
    /// [`Frame::Respond`] / [`Frame::StreamEnd`] or the outbound
    /// [`Frame::Cancel`].
    outgoing: HashMap<u64, OutgoingKind>,
    /// Calls the remote process sent to a host here. Leave on the
    /// outbound [`Frame::Respond`] / [`Frame::StreamEnd`], or when the
    /// hosted call is cancelled before responding.
    incoming: HashSet<u64>,
}

/// The per-process runtime.
///
/// One [`Runtime`] instance owns every routing table, host dispatcher
/// and early-event store for a process. It is built via [`Runtime::init`]
/// (or [`Runtime::mock`] in tests) and stashed in a process-global
/// [`OnceLock`] reachable through [`Runtime::global`].
///
/// The runtime is deliberately transport-agnostic: it produces outbound
/// [`Envelope`]s on a channel and accepts inbound bytes through
/// [`Runtime::inject_wire_envelope`]. The [`istmo-android`] and
/// [`istmo-ios`] crates provide the JNI / FFI pumps that connect it to
/// the native side.
///
/// # Threads
///
/// No hosted call creates a thread. Each runs where its
/// [`DispatchMode`] puts it: [`DispatchMode::Inline`] work starts on the
/// thread that delivered it and, once it waits, continues on a few
/// persistent async workers that never block (a waiting future does not
/// hold one, so their number cannot deadlock anything);
/// [`DispatchMode::Isolated`] work runs on persistent blocking workers,
/// reused when idle and started when none is, so no call waits behind
/// busy ones. Hosts on blocking workers may block and call back into the
/// runtime and wait, nested as deep as they like.
///
/// A host thread that would wait on a full channel (the outbound
/// envelope channel, or a local consumer's bounded stream) gives up as
/// soon as its call is cancelled, so neither a consumer that stopped
/// reading nor a transport that stopped draining after
/// [`shutdown`](Self::shutdown) pins it forever.
///
/// A few threads never wait on a full outbound channel, because the
/// channel's progress may depend on them: the async workers (for a
/// synchronous send; async code awaits room instead), and any thread
/// inside a [`DeliveryScope`] — the transports' pumps, a process
/// bridge's reader ([`inject_wire_envelope`](Self::inject_wire_envelope)),
/// a thread submitting frames to a sink, and whatever they run in
/// place, inline hosts included. Their sends queue behind the channel
/// in order and a drain thread moves them on as room frees up, so a
/// host cannot deadlock waiting for the very thread that drains the
/// channel. That queue has no bound;
/// [`outbound_spill`](Self::outbound_spill) counts it. Every other
/// thread, an app thread or a Kotlin / Swift caller polling an inline
/// host included, waits for room: the backpressure the channel gives.
/// While frames are queued that way, it waits for its own frame to
/// leave the queue.
///
/// Cancels, handle releases and the ends of cancelled streams never
/// wait, from any thread: they run from `Drop` and must not be lost, so
/// they queue the same way. A transport that stops draining while a
/// thread keeps dropping handles therefore grows that queue without
/// bound; past four channels' worth it logs a warning (at most every
/// 10 s), and [`outbound_spill`](Self::outbound_spill) shows it.
///
/// That covers the outbound channel only. An inbound stream item or end
/// for a local consumer with a full buffer is delivered by the thread
/// that injects it: a plain thread or a bridge's reader waits for room;
/// the platform transports offer such items and ends first and move
/// them to a thread that may wait
/// ([`offer_peer_event`](Self::offer_peer_event),
/// [`offer_peer_stream_end`](Self::offer_peer_stream_end)). Rust-hosted
/// streams await room on the async workers.
///
/// # Locking
///
/// No mutex of the runtime is acquired while another one is held: every
/// critical section is a single map or slot operation, and the remote
/// sink, release hooks and channel sends run with no runtime lock held.
/// The only nesting is the hosted-call table cancelling a
/// [`CancelToken`], which takes that token's own lock inside, and
/// [`Runtime::init`] replaying the pre-start buffer into the early-event
/// store while holding the buffer's lock. Keep it that way: a new nested
/// acquisition needs a documented order.
///
/// [`istmo-android`]: https://docs.rs/istmo-android
/// [`istmo-ios`]: https://docs.rs/istmo-ios
pub struct Runtime {
    outbound: FlumeSender<Envelope>,
    routing: RoutingTables,
    early_events: Arc<EarlyEventStore>,
    main_thread: Arc<dyn MainThread>,
    next_id: AtomicU64,
    next_native_handle: AtomicU64,
    shutting_down: AtomicBool,
    /// Cancelled by [`shutdown`](Self::shutdown): frees senders waiting
    /// on a full outbound channel the transport stopped draining.
    stopped: CancelToken,

    declared_plugins: Mutex<HashSet<&'static str>>,

    enforce_declarations: AtomicBool,

    hosts: Mutex<HashMap<&'static str, Arc<dyn Dispatch>>>,

    /// Typed in-process shortcut slots.
    ///
    /// Each entry stores a boxed `Arc<dyn <Trait>Typed>` for a plugin id,
    /// erased as `Arc<dyn Any + Send + Sync>`. The generated client of a
    /// plugin can downcast the slot back to the exact `Arc<dyn …Typed>`
    /// it knows about and call through it directly, bypassing every
    /// wire step (encode, routing, outbound, decode).
    typed_hosts: Mutex<HashMap<&'static str, Arc<dyn Any + Send + Sync>>>,

    /// Short-circuit flag for the typed-shortcut lookup.
    ///
    /// Set to `true` the first time a typed host is registered; the
    /// generated client checks this atomic before taking
    /// [`typed_hosts`](Self::typed_hosts)' mutex so a process that
    /// installs no shortcut at all pays roughly one atomic load per
    /// call instead of a mutex + hash-map miss.
    has_typed_hosts: AtomicBool,

    hosted: Mutex<HostedCalls>,

    remote_plugins: Mutex<HashSet<String>>,

    remote_calls: Mutex<RemoteCalls>,

    remote_instances: Mutex<HashSet<InstanceId>>,

    remote_sink: Mutex<Option<RemoteEnvelopeSink>>,

    native_handle_release_hooks: Mutex<Vec<NativeHandleReleaseHook>>,

    /// Open [`FrameSink`]s, by raw id.
    frame_sinks: FrameSinks,

    /// Delivers calls to the peer on the caller's thread; see
    /// [`install_direct_peer`](Self::install_direct_peer).
    direct_peer: OnceLock<Arc<dyn DirectPeer>>,

    /// Frames put on the outbound channel and not yet reported delivered
    /// by the transport ([`outbound_delivered`](Self::outbound_delivered)).
    outbound_backlog: Arc<AtomicUsize>,

    /// The transport reports every delivered frame
    /// ([`track_outbound_deliveries`](Self::track_outbound_deliveries)).
    tracks_deliveries: AtomicBool,

    /// Frames a thread that must not block could not put on the full
    /// outbound channel, on their way to it in order.
    spill: Arc<spill::Spill>,

    /// Credit lanes of peer streams opened with a window
    /// ([`serve_peer_stream`](Self::serve_peer_stream)).
    peer_credits: Mutex<HashMap<StreamId, CreditLane>>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .field("main_thread", &self.main_thread)
            .field("shutting_down", &self.is_shut_down())
            .finish_non_exhaustive()
    }
}

static GLOBAL: OnceLock<Arc<Runtime>> = OnceLock::new();

/// Early events submitted by the platform before [`Runtime::init`]
/// ran — e.g. a share intent or deep link delivered to an `Activity`
/// whose `onCreate` executes before Rust boots. Replayed in order by
/// [`Runtime::init`].
static PRE_START_EARLY_EVENTS: Mutex<VecDeque<(String, EarlyEventKind, Vec<u8>)>> =
    Mutex::new(VecDeque::new());

/// Most early events buffered before [`Runtime::init`]; the oldest entry is
/// dropped on overflow so a runtime that never starts cannot grow it
/// without bound.
pub const PRE_START_EARLY_EVENT_CAPACITY: usize = 64;

impl Runtime {
    /// Build a runtime and install it as the process-global singleton.
    ///
    /// Called once at app startup by the platform transport, typically
    /// through the [`istmo::runtime!`](../../istmo_macros/macro.runtime.html)
    /// macro. Early events buffered by [`Runtime::submit_early_event`]
    /// are replayed, in submission order, before any event submitted
    /// after this call.
    ///
    /// # Errors
    ///
    /// Returns [`IstmoError::RuntimeAlreadyStarted`] if another
    /// [`Runtime`] has already been installed in this process.
    // The buffer lock is held across `GLOBAL.set` and the replay so a
    // concurrent `submit_early_event` either lands in the buffer before
    // the drain or sees the runtime after it — never in between.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "lock spans the install and the replay on purpose"
    )]
    pub fn init(config: RuntimeConfig) -> Result<RuntimeInit, IstmoError> {
        let init = build(config);
        let mut pending = lock(&PRE_START_EARLY_EVENTS);
        GLOBAL
            .set(Arc::clone(&init.runtime))
            .map_err(|_| IstmoError::RuntimeAlreadyStarted)?;
        for (channel, kind, payload) in pending.drain(..) {
            init.runtime.publish_early(&channel, &kind, payload);
        }
        drop(pending);
        Ok(init)
    }

    /// Deliver an early event to the process-global runtime, or buffer
    /// it until [`Runtime::init`] runs.
    ///
    /// Platform transports route `publishEarlyQueue` /
    /// `publishEarlyLatest` through this so events raised before Rust
    /// boots (cold-start intents, share hand-offs) are not lost. At
    /// most [`PRE_START_EARLY_EVENT_CAPACITY`] events are buffered.
    ///
    /// # Errors
    ///
    /// Whatever [`Runtime::dispatch_inbound`] returns once the runtime
    /// exists.
    // The lock must stay held across the `GLOBAL` check so `init`
    // cannot drain the buffer between the check and the push.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "lock spans the GLOBAL check on purpose"
    )]
    pub fn submit_early_event(
        channel: String,
        kind: EarlyEventKind,
        payload: Vec<u8>,
    ) -> Result<(), IstmoError> {
        let mut pending = lock(&PRE_START_EARLY_EVENTS);
        if let Some(runtime) = GLOBAL.get() {
            drop(pending);
            return runtime.dispatch_inbound(Envelope::new(Frame::EarlyEvent {
                channel,
                kind,
                payload,
            }));
        }
        if pending.len() == PRE_START_EARLY_EVENT_CAPACITY {
            pending.pop_front();
        }
        pending.push_back((channel, kind, payload));
        Ok(())
    }

    /// Return a clone of the process-global runtime.
    ///
    /// # Errors
    ///
    /// Returns [`IstmoError::RuntimeNotStarted`] if [`Runtime::init`]
    /// has not been called yet.
    pub fn global() -> Result<Arc<Self>, IstmoError> {
        GLOBAL.get().cloned().ok_or(IstmoError::RuntimeNotStarted)
    }

    /// Build a runtime for tests, without touching the process-global
    /// slot.
    ///
    /// Wired with [`RuntimeConfig::inline`] — main-thread tasks run
    /// synchronously on the caller.
    #[must_use]
    pub fn mock() -> RuntimeInit {
        build(RuntimeConfig::inline())
    }

    /// Access the runtime's [`MainThread`] adapter.
    #[must_use]
    pub const fn main_thread(&self) -> &Arc<dyn MainThread> {
        &self.main_thread
    }

    /// Access the runtime's early-event store.
    #[must_use]
    pub const fn early_events(&self) -> &Arc<EarlyEventStore> {
        &self.early_events
    }

    #[must_use]
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Allocate a fresh [`NativeHandleId`] for an object owned by a
    /// Rust-side host.
    ///
    /// Ids count up from [`NativeHandleId::RUST_ALLOCATED_BASE`], a
    /// range the Kotlin and Swift runtimes never reach, so ids from both
    /// sides can share one handle space.
    #[must_use]
    pub fn alloc_native_handle_id(&self) -> NativeHandleId {
        NativeHandleId(self.next_native_handle.fetch_add(1, Ordering::Relaxed))
    }

    /// `true` once [`shutdown`](Self::shutdown) has run.
    #[must_use]
    pub fn is_shut_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    fn ensure_running(&self) -> Result<(), IstmoError> {
        if self.is_shut_down() {
            Err(IstmoError::RuntimeShutDown)
        } else {
            Ok(())
        }
    }

    /// Undo the registration under `id` when [`shutdown`](Self::shutdown)
    /// ran since the caller's [`ensure_running`](Self::ensure_running).
    ///
    /// `shutdown` raises the flag before clearing the routing tables, and
    /// both sides go through the routing lock, so a registration either
    /// lands before the clear (and is cleared) or sees the flag here.
    fn recheck_running(&self, id: u64) -> Result<(), IstmoError> {
        if self.is_shut_down() {
            self.routing.remove_pending(id);
            return Err(IstmoError::RuntimeShutDown);
        }
        Ok(())
    }

    fn host(&self, plugin_id: &str) -> Option<Arc<dyn Dispatch>> {
        lock(&self.hosts).get(plugin_id).cloned()
    }

    /// Invoke a plugin method and return a [`CallHandle`] that resolves
    /// to the response.
    ///
    /// The handle implements [`Future`] and can be `.await`ed directly;
    /// it can also be resolved synchronously with
    /// [`CallHandle::recv_blocking`]. Dropping the handle before the
    /// response arrives cancels the call.
    ///
    /// Method arguments are passed as pre-encoded wire `payload`
    /// bytes — usually produced by the client generated by
    /// [`#[plugin]`](../../istmo_macros/attr.plugin.html), so callers
    /// rarely construct these frames by hand.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::RuntimeShutDown`] after [`shutdown`](Self::shutdown),
    ///   including while waiting for room on a full outbound channel.
    /// - [`IstmoError::ChannelClosed`] if the outbound channel is gone.
    /// - [`IstmoError::Codec`] if the remote sink could not encode the
    ///   envelope.
    pub fn call(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        self.ensure_running()?;
        let plugin_id = plugin_id.into();
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
        self.recheck_running(call_id.get())?;
        let host = self.host(&plugin_id);
        let hosted = host.is_some();
        if let Some(dispatcher) = host {
            self.dispatch_hosted_call(
                dispatcher,
                HostedCall {
                    origin: Origin::Local,
                    call_id,
                    instance_id,
                    method: method.into(),
                    payload,
                },
            );
        } else {
            let method = method.into();
            if !self.call_peer_directly(call_id, &plugin_id, instance_id, &method, &payload) {
                self.send_or_unregister(
                    call_id.get(),
                    Envelope::new(Frame::Call {
                        call_id,
                        plugin_id,
                        instance_id,
                        method,
                        payload,
                    }),
                )?;
            }
        }
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
            hosted,
        })
    }

    /// Hand a registered call to the [`DirectPeer`] when one is installed
    /// and nothing waits in the outbound channel. `false`: not taken, it
    /// goes through the channel.
    fn call_peer_directly(
        &self,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: &str,
        payload: &[u8],
    ) -> bool {
        let Some(peer) = self.direct_peer.get() else {
            return false;
        };
        // A frame queued earlier (a notification, a cancel) must reach
        // the peer first; calls to another process go through its sink.
        if self.outbound_backlog.load(Ordering::Acquire) != 0 || self.is_remote_plugin(plugin_id) {
            return false;
        }
        match peer.call(call_id, plugin_id, instance_id, method, payload) {
            DirectOutcome::Answered(result) => {
                if let Err(err) = self.routing.complete(call_id, result) {
                    tracing::debug!(?err, ?call_id, "direct answer after its caller left");
                }
                true
            }
            DirectOutcome::Pending => true,
            DirectOutcome::Unavailable => false,
        }
    }

    /// Install the transport's [`DirectPeer`]: calls to plugins with no
    /// host here are then handed to the peer on the calling thread
    /// whenever the outbound channel is drained. Once per runtime; later
    /// installs are ignored.
    ///
    /// A transport installing one reports every frame it took off the
    /// outbound channel with [`outbound_delivered`](Self::outbound_delivered)
    /// once delivered; until it does, calls keep the channel.
    pub fn install_direct_peer(&self, peer: Arc<dyn DirectPeer>) {
        self.track_outbound_deliveries();
        if self.direct_peer.set(peer).is_err() {
            tracing::warn!("a direct peer is already installed; ignored");
        }
    }

    /// Declare that the transport reports every frame it takes off the
    /// outbound channel with [`outbound_delivered`](Self::outbound_delivered)
    /// once delivered, from before the first frame goes out. An answer
    /// [`serve_peer_call`](Self::serve_peer_call) would hand back directly
    /// then follows as a frame instead while anything queued before it (a
    /// notification or event the host sent first) is still on its way.
    /// Implied by [`install_direct_peer`](Self::install_direct_peer).
    pub fn track_outbound_deliveries(&self) {
        self.tracks_deliveries.store(true, Ordering::Release);
    }

    /// Frames this runtime queued behind its full outbound channel,
    /// because the thread sending them must never wait (see the
    /// type's `# Threads`): now, at most, and in all. `queued` growing
    /// means such a thread sends faster than the transport drains; the
    /// runtime also logs a warning past four channels' worth.
    #[must_use]
    pub fn outbound_spill(&self) -> SpillStats {
        self.spill.stats()
    }

    /// Frames sent and not yet reported delivered, for a transport that
    /// reports them.
    fn frames_on_their_way(&self) -> bool {
        self.tracks_deliveries.load(Ordering::Acquire)
            && self.outbound_backlog.load(Ordering::Acquire) != 0
    }

    /// The transport delivered one frame it took off the outbound
    /// channel. See [`install_direct_peer`](Self::install_direct_peer).
    pub fn outbound_delivered(&self) {
        // Saturating: a transport reporting more than it took must not
        // wrap the count and stall direct calls forever.
        let mut current = self.outbound_backlog.load(Ordering::Acquire);
        while let Some(next) = current.checked_sub(1) {
            match self.outbound_backlog.compare_exchange_weak(
                current,
                next,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return,
                Err(actual) => current = actual,
            }
        }
    }

    /// Open a stream-returning plugin method.
    ///
    /// Returns a [`StreamHandle`] whose [`recv`](StreamHandle::recv)
    /// blocks on the next item until the peer signals end-of-stream.
    /// `capacity` controls how many items may buffer before the producer
    /// blocks; `0` buffers without limit. The block lands on whichever
    /// thread delivers the item (the hosted producer's pump, or the
    /// platform thread that submitted the inbound event).
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    pub fn stream(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
        capacity: usize,
    ) -> Result<StreamHandle, IstmoError> {
        self.ensure_running()?;
        let plugin_id = plugin_id.into();
        let id = self.next_id();
        let call_id = CallId(id);
        let stream_id = StreamId(id);
        let rx = self.routing.register_stream(stream_id, capacity);
        self.recheck_running(id)?;
        let host = self.host(&plugin_id);
        let hosted = host.is_some();
        if let Some(dispatcher) = host {
            self.dispatch_hosted_call(
                dispatcher,
                HostedCall {
                    origin: Origin::Local,
                    call_id,
                    instance_id,
                    method: method.into(),
                    payload,
                },
            );
        } else {
            self.send_or_unregister(
                id,
                Envelope::new(Frame::Call {
                    call_id,
                    plugin_id,
                    instance_id,
                    method: method.into(),
                    payload,
                }),
            )?;
        }
        Ok(StreamHandle {
            stream_id,
            receiver: rx,
            runtime: Arc::downgrade(self),
            hosted,
            ended: AtomicBool::new(false),
        })
    }

    /// Ask a stateful plugin to construct a new instance.
    ///
    /// The returned [`CallHandle`] resolves to the [`InstanceId`] of the
    /// freshly created instance. Subsequent calls carry that instance id
    /// to route methods to the correct object on the peer side.
    ///
    /// # Errors
    ///
    /// As [`call`](Self::call).
    pub fn create_instance(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        self.ensure_running()?;
        let plugin_id = plugin_id.into();
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
        self.recheck_running(call_id.get())?;
        let host = self.host(&plugin_id);
        let hosted = host.is_some();
        if let Some(dispatcher) = host {
            self.dispatch_hosted_create_instance(
                dispatcher,
                Origin::Local,
                call_id,
                plugin_id,
                payload,
            );
        } else {
            self.send_or_unregister(
                call_id.get(),
                Envelope::new(Frame::CreateInstance {
                    call_id,
                    plugin_id,
                    payload,
                }),
            )?;
        }
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
            hosted,
        })
    }

    /// Send the request frame of a call registered under `id`, dropping
    /// the registration again if it cannot be sent.
    fn send_or_unregister(&self, id: u64, envelope: Envelope) -> Result<(), IstmoError> {
        self.send_outbound(envelope).inspect_err(|_| {
            self.routing.remove_pending(id);
            lock(&self.remote_calls).outgoing.remove(&id);
        })
    }

    /// Record a locally-hosted plugin instance in the routing tables so
    /// subsequent inbound calls can find it.
    pub fn register_instance(&self, instance_id: InstanceId, plugin_id: impl Into<String>) {
        self.routing
            .register_instance(instance_id, plugin_id.into());
    }

    /// Plugin id a registered instance belongs to, if it is registered.
    #[must_use]
    pub fn instance_plugin_id(&self, instance_id: InstanceId) -> Option<String> {
        self.routing.instance(instance_id)
    }

    /// Release a previously created plugin instance and notify the peer.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::RuntimeShutDown`] if [`shutdown`](Self::shutdown)
    ///   runs while waiting for room on a full outbound channel.
    /// - [`IstmoError::ChannelClosed`] if the outbound channel is gone.
    /// - [`IstmoError::Codec`] if the remote sink could not encode the
    ///   envelope.
    pub fn destroy_instance(self: &Arc<Self>, instance_id: InstanceId) -> Result<(), IstmoError> {
        let plugin_id = self.routing.instance(instance_id);
        let hosted = plugin_id
            .as_deref()
            .is_some_and(|pid| lock(&self.hosts).contains_key(pid));
        if hosted {
            self.dispatch_hosted_destroy_instance(Origin::Local, instance_id);
            return Ok(());
        }
        self.routing.remove_instance(instance_id);
        self.send_outbound(Envelope::new(Frame::DestroyInstance { instance_id }))
    }

    /// Notify the peer that it can drop the platform object backing
    /// `handle_id`.
    ///
    /// Called from the [`Drop`] impl of [`NativeHandle`](crate::NativeHandle),
    /// so it never blocks on a full outbound channel.
    ///
    /// Every hook registered via
    /// [`install_native_handle_release_hook`](Self::install_native_handle_release_hook)
    /// fires before the wire frame is enqueued — hooks are the release
    /// route for Rust-side hosts whose handles are consumed by clients
    /// living inside the same process (no external transport pump).
    ///
    /// # Errors
    ///
    /// - [`IstmoError::ChannelClosed`] when the outbound pump is gone.
    ///
    /// A full outbound channel never fails it: the release queues
    /// behind the channel.
    pub fn release_native_handle(&self, handle_id: NativeHandleId) -> Result<(), IstmoError> {
        let hooks: Vec<NativeHandleReleaseHook> = lock(&self.native_handle_release_hooks).clone();
        for hook in &hooks {
            hook(handle_id);
        }
        self.try_send_outbound(Envelope::new(Frame::ReleaseNativeHandle { handle_id }))
    }

    /// Register a local callback that fires for every
    /// [`Frame::ReleaseNativeHandle`] emitted by
    /// [`release_native_handle`](Self::release_native_handle).
    ///
    /// Multiple hooks are supported and all fire on every release.
    /// Handles allocated by a Rust-side host register a hook so they can
    /// free the URI / URL / path map entry when the client drops its
    /// [`NativeHandle`](crate::NativeHandle), without depending on an
    /// external transport pump to route the release back into the
    /// process.
    ///
    /// Hooks fire *in addition* to the outbound wire frame — mobile
    /// transports (JNI pump on Android, FFI callbacks on iOS) still
    /// receive the release the same way. Hooks that inspect handle ids
    /// not owned by their host should be no-ops.
    pub fn install_native_handle_release_hook(&self, hook: NativeHandleReleaseHook) {
        lock(&self.native_handle_release_hooks).push(hook);
    }

    /// Fire-and-forget one-way call.
    ///
    /// No response is expected and no [`CallHandle`] is returned. Useful
    /// for lifecycle notifications and other observer patterns where
    /// dropping the ack round-trip is worth the latency saving.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::RuntimeShutDown`] after [`shutdown`](Self::shutdown),
    ///   including while waiting for room on a full outbound channel.
    /// - [`IstmoError::ChannelClosed`] when the outbound pump is gone.
    pub fn notify(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<(), IstmoError> {
        self.ensure_running()?;
        let plugin_id = plugin_id.into();
        let method = method.into();
        if let Some(dispatcher) = self.host(&plugin_id) {
            dispatch_notify(dispatcher, Origin::Local, instance_id, method, payload);
            return Ok(());
        }
        self.send_outbound(Envelope::new(Frame::Notify {
            plugin_id,
            instance_id,
            method,
            payload,
        }))
    }

    /// Signal cancellation of an in-flight call to the peer.
    ///
    /// Sends [`Frame::Cancel`] only while the call is still pending
    /// here, and never blocks: it runs from [`CallHandle`]'s [`Drop`].
    ///
    /// # Errors
    ///
    /// - [`IstmoError::ChannelClosed`] if the outbound pump is gone.
    ///
    /// A full outbound channel never fails it: the cancel queues behind
    /// the channel, so the peer always hears it.
    pub fn cancel_call(&self, call_id: CallId) -> Result<(), IstmoError> {
        if !self.routing.remove_pending(call_id.get()) {
            return Ok(());
        }
        self.try_send_outbound(Envelope::new(Frame::Cancel { call_id }))
    }

    /// Cancel a call or stream served by a host registered on this
    /// runtime: trips the [`CancelToken`] handed to the hosted dispatch
    /// instead of sending [`Frame::Cancel`] to the peer, which never saw
    /// the call.
    fn cancel_hosted_local_call(&self, call_id: CallId) {
        self.routing.remove_pending(call_id.get());
        lock(&self.hosted).cancel(Origin::Local, call_id);
    }

    /// Ask the peer to close a live stream.
    ///
    /// Sends [`Frame::Cancel`] only while the stream is still open
    /// here, and never blocks: it runs from [`StreamHandle`]'s [`Drop`].
    ///
    /// # Errors
    ///
    /// As [`cancel_call`](Self::cancel_call).
    pub fn cancel_stream(&self, stream_id: StreamId) -> Result<(), IstmoError> {
        self.cancel_call(CallId(stream_id.get()))
    }

    /// Decode a wire envelope from a runtime in another process and
    /// dispatch it.
    ///
    /// The receive side of a process bridge (see
    /// [`install_remote_envelope_sink`](Self::install_remote_envelope_sink)).
    /// Calls it carries are served under their own origin: their ids may
    /// coincide with ids the platform transport uses, and their
    /// responses, events and stream ends go back through the remote sink
    /// (or the outbound channel while no sink is installed).
    ///
    /// # Errors
    ///
    /// Any variant of [`IstmoError`] surfaced by
    /// [`Envelope::from_wire_bytes`] or [`Runtime::dispatch_inbound`].
    ///
    /// # Threads
    ///
    /// The bridge's reader calls this, and the other process may be
    /// waiting for that reader: the call runs inside a
    /// [`DeliveryScope`], so nothing it sends (an inline host's frames,
    /// an error answer) waits for room on the outbound channel.
    ///
    /// What it delivers does wait: a [`Frame::Event`] or
    /// [`Frame::StreamEnd`] for a local consumer whose buffer is full
    /// holds the reader until the consumer takes one. That is the only
    /// backpressure a remote producer gets (the bridge carries no
    /// credit). A consumer that, between two items, waits for something
    /// else coming over the same bridge must therefore not let its
    /// buffer fill up.
    pub fn inject_wire_envelope(self: &Arc<Self>, bytes: &[u8]) -> Result<(), IstmoError> {
        let _reader = DeliveryScope::enter();
        let envelope = Envelope::from_wire_bytes(bytes)?;
        self.dispatch_from(Origin::Remote, envelope)
    }

    /// Dispatch an already-decoded envelope from the peer.
    ///
    /// Routes call frames to the matching registered dispatcher,
    /// completes pending [`CallHandle`]s from response frames, forwards
    /// stream events, and so on. A call for a plugin with no registered
    /// host is answered with an error response rather than an `Err`
    /// here; so is any call arriving after [`shutdown`](Self::shutdown).
    /// An error response to a stream call ends that stream with
    /// [`StreamEndReason::Error`].
    ///
    /// # Errors
    ///
    /// - [`IstmoError::ProtocolVersionMismatch`] for a frame from another
    ///   protocol version.
    /// - [`IstmoError::UnknownCallId`] / [`IstmoError::UnknownStreamId`]
    ///   when a response, event or stream end references nothing pending
    ///   (usually a late frame after a cancel).
    /// - [`IstmoError::RoutingMismatch`] when it lands on the wrong kind
    ///   of registration.
    /// - [`IstmoError::ChannelClosed`] when the stream's consumer is gone.
    pub fn dispatch_inbound(self: &Arc<Self>, envelope: Envelope) -> Result<(), IstmoError> {
        self.dispatch_from(Origin::Peer, envelope)
    }

    fn dispatch_from(
        self: &Arc<Self>,
        origin: Origin,
        envelope: Envelope,
    ) -> Result<(), IstmoError> {
        if envelope.version != PROTOCOL_VERSION {
            return Err(IstmoError::ProtocolVersionMismatch {
                expected: PROTOCOL_VERSION,
                got: envelope.version,
            });
        }
        match envelope.frame {
            Frame::Respond { call_id, result } => {
                self.settle_outgoing_remote(call_id.get(), Some(&result));
                self.routing.deliver_response(call_id, result)
            }
            Frame::Event { stream_id, payload } => self.routing.deliver_event(stream_id, payload),
            Frame::StreamEnd { stream_id, reason } => {
                self.settle_outgoing_remote(stream_id.get(), None);
                self.routing.deliver_stream_end(stream_id, reason)
            }
            Frame::Call {
                call_id,
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                match self.host(&plugin_id) {
                    Some(dispatcher) => self.dispatch_hosted_call(
                        dispatcher,
                        HostedCall {
                            origin,
                            call_id,
                            instance_id,
                            method,
                            payload,
                        },
                    ),
                    None => self.reject_unhosted(origin, call_id, &plugin_id, "Call"),
                }
                Ok(())
            }
            Frame::Cancel { call_id } => {
                lock(&self.hosted).cancel(origin, call_id);
                Ok(())
            }
            Frame::EarlyEvent {
                channel,
                kind,
                payload,
            } => {
                self.publish_early(&channel, &kind, payload);
                Ok(())
            }
            Frame::Notify {
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                if self.is_shut_down() {
                    tracing::debug!(plugin_id = %plugin_id, "dropped inbound Notify after shutdown");
                } else if let Some(dispatcher) = self.host(&plugin_id) {
                    dispatch_notify(dispatcher, origin, instance_id, method, payload);
                } else {
                    tracing::warn!(plugin_id = %plugin_id, "inbound Notify for unregistered plugin");
                }
                Ok(())
            }
            Frame::CreateInstance {
                call_id,
                plugin_id,
                payload,
            } => {
                match self.host(&plugin_id) {
                    Some(dispatcher) => self.dispatch_hosted_create_instance(
                        dispatcher, origin, call_id, plugin_id, payload,
                    ),
                    None => self.reject_unhosted(origin, call_id, &plugin_id, "CreateInstance"),
                }
                Ok(())
            }
            Frame::DestroyInstance { instance_id } => {
                self.dispatch_hosted_destroy_instance(origin, instance_id);
                Ok(())
            }
            Frame::ReleaseNativeHandle { .. } => {
                tracing::warn!("dropped inbound frame with outbound-only variant");
                Ok(())
            }
        }
    }

    fn refuse_origin(&self, origin: Origin, call_id: CallId, plugin_id: &str) {
        tracing::warn!(plugin_id = %plugin_id, ?origin, "hosted request refused for its origin");
        exec::finish_now(self.respond(
            origin,
            call_id,
            Err(encode_dispatch_error_string(&format!(
                "`{plugin_id}` refuses requests from this origin"
            ))),
            Wait::Block.for_sync(),
        ));
    }

    fn reject_unhosted(&self, origin: Origin, call_id: CallId, plugin_id: &str, frame: &str) {
        tracing::warn!(plugin_id = %plugin_id, frame, "inbound request for unregistered plugin");
        exec::finish_now(self.respond(
            origin,
            call_id,
            Err(encode_dispatch_error_string(&format!(
                "no host registered for `{plugin_id}`"
            ))),
            Wait::Block.for_sync(),
        ));
    }

    /// Register a Rust-hosted plugin dispatcher.
    ///
    /// Called through [`RuntimeInit::host`] (and so `runtime!(hosts: …)`)
    /// during startup. A host generated by `#[istmo::plugin]` is also
    /// installed as the plugin's typed in-process shortcut
    /// ([`Dispatch::typed_shortcut`]): this process's generated clients
    /// then call the implementation directly, polling its futures on the
    /// caller's executor, instead of encoding the call. Calls that take
    /// the wire path (from Kotlin, Swift or another process, and
    /// methods marked `#[istmo::blocking]`) run where
    /// [`Dispatch::method_mode`] says: on the delivering thread for
    /// [`DispatchMode::Inline`], on a blocking worker for
    /// [`DispatchMode::Isolated`].
    /// [`register_host_wire_only`](Self::register_host_wire_only) keeps
    /// every call on the wire path and on a blocking worker.
    pub fn register_host<D: Dispatch>(self: &Arc<Self>, dispatcher: D) {
        self.register_host_arc(Arc::new(dispatcher));
    }

    /// [`register_host`](Self::register_host) for an already-shared
    /// dispatcher.
    pub fn register_host_arc<D: Dispatch>(self: &Arc<Self>, dispatcher: Arc<D>) {
        let plugin_id = dispatcher.plugin_id();
        if let Some(typed) = Arc::clone(&dispatcher).typed_shortcut() {
            lock(&self.typed_hosts).insert(plugin_id, typed);
            self.has_typed_hosts.store(true, Ordering::Release);
        }
        self.install_host(dispatcher);
    }

    /// Register `dispatcher` for the wire path only: every call, from
    /// this process too, is encoded and served on a blocking worker
    /// ([`DispatchMode::Isolated`]), whatever modes the dispatcher
    /// declares; so are its notifications and instance creation. The
    /// opt-out for a host whose methods block or need a thread of their
    /// own throughout.
    pub fn register_host_wire_only<D: Dispatch>(self: &Arc<Self>, dispatcher: D) {
        let dispatcher = Arc::new(WireOnly(dispatcher));
        lock(&self.typed_hosts).remove(dispatcher.plugin_id());
        self.install_host(dispatcher);
    }

    fn install_host<D: Dispatch>(self: &Arc<Self>, dispatcher: Arc<D>) {
        let plugin_id = dispatcher.plugin_id();
        let dispatcher: Arc<dyn Dispatch> = dispatcher;
        dispatcher.runtime_attached(Arc::downgrade(self));
        lock(&self.hosts).insert(plugin_id, dispatcher);
    }

    /// Install a typed shortcut for an in-process plugin.
    ///
    /// The generated client of a plugin trait looks this slot up on every
    /// call and, when the stored shortcut matches the trait it knows
    /// about, calls through it directly — no encode, no routing, no
    /// outbound channel, no decode. The wire path stays live in parallel
    /// (through [`register_host`](Self::register_host)) so remote callers
    /// and cross-process bridges keep working.
    ///
    /// `T` is the plugin's shortcut trait object type (a `dyn …Typed`
    /// generated by [`#[istmo::plugin]`](../../istmo_macros/attr.plugin.html));
    /// `plugin_id` is the string identifier the trait is registered
    /// under. The shortcut is only safe to install when the trait's
    /// methods are non-blocking and non-reentrant, same contract as
    /// [`DispatchMode::Inline`].
    pub fn register_typed_host<T>(&self, plugin_id: &'static str, typed: Arc<T>)
    where
        T: ?Sized + Send + Sync + 'static,
    {
        let holder: Arc<dyn Any + Send + Sync> = Arc::new(typed);
        lock(&self.typed_hosts).insert(plugin_id, holder);
        self.has_typed_hosts.store(true, Ordering::Release);
    }

    /// Look up a typed shortcut by plugin id.
    ///
    /// Returns the installed `Arc<T>` when the slot is present and `T`
    /// matches the exact trait object stored there (same vtable). The
    /// generated client calls this on every method invocation; keep the
    /// `T` parameter identical to the one passed to
    /// [`register_typed_host`](Self::register_typed_host).
    #[must_use]
    pub fn typed_host<T>(&self, plugin_id: &'static str) -> Option<Arc<T>>
    where
        T: ?Sized + Send + Sync + 'static,
    {
        // Register stores an `Arc<T>` erased behind one extra `Arc` layer
        // so the holder itself can be stored as `Arc<dyn Any + Send +
        // Sync>` (`Arc<T>` is `Sized + Any`, but `dyn T` typically is
        // not). Cloning from the map keeps the inner `Arc<T>` alive even
        // if `shutdown` clears the slot before the caller gets to use
        // it.
        if !self.has_typed_hosts.load(Ordering::Acquire) {
            return None;
        }
        let holder = lock(&self.typed_hosts).get(plugin_id).cloned()?;
        let outer = holder.downcast::<Arc<T>>().ok()?;
        Some(Arc::clone(&outer))
    }

    /// Open a [`FrameSink`] whose frames carry metadata `M`.
    ///
    /// Pass [`FrameSink::id`] to the producer as a plugin method
    /// argument. A Rust producer in this runtime resolves it with
    /// [`frame_sender`](Self::frame_sender); Kotlin and Swift producers
    /// submit through a [`NativeProducer`] the transport opens with
    /// [`open_native_producer`](Self::open_native_producer).
    ///
    /// # Errors
    ///
    /// [`IstmoError::RuntimeShutDown`] after [`shutdown`](Self::shutdown).
    pub fn open_frame_sink<M: Message + Send + 'static>(
        self: &Arc<Self>,
        policy: SinkPolicy,
    ) -> Result<FrameSink<M>, IstmoError> {
        self.ensure_running()?;
        let sink = self
            .frame_sinks
            .open(self.next_id(), policy, Arc::downgrade(self));
        // `shutdown` raises its flag before closing every sink: a sink
        // registered after that close is caught here (and closed by its
        // own drop).
        if self.is_shut_down() {
            return Err(IstmoError::RuntimeShutDown);
        }
        Ok(sink)
    }

    /// The producer end of the sink `id`, for a Rust host in this
    /// process. No bytes are involved: frames are moved.
    ///
    /// Every sender counts as one producer; the sink ends as
    /// [`EndReason::ProducerGone`](crate::native_frame::EndReason::ProducerGone)
    /// when the last one drops.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::UnknownFrameSink`] when no such sink is open here
    ///   (closed, or opened by another runtime or process).
    /// - [`IstmoError::FrameSinkTypeMismatch`] when the sink carries
    ///   another metadata type.
    pub fn frame_sender<M: Message + Send + 'static>(
        &self,
        id: FrameSinkId<M>,
    ) -> Result<FrameSender<M>, IstmoError> {
        self.frame_sinks
            .sender(id.raw())
            .map_err(|lookup| match lookup {
                SenderLookup::Unknown => IstmoError::UnknownFrameSink(id.raw()),
                SenderLookup::TypeMismatch { found } => IstmoError::FrameSinkTypeMismatch {
                    id: id.raw(),
                    expected: std::any::type_name::<M>(),
                    found,
                },
            })
    }

    /// Open a producer on the sink `sink` for a Kotlin or Swift host:
    /// what the transports' `IstmoFrameSender` holds. `fingerprint` is
    /// [`meta_fingerprint`](crate::native_frame::meta_fingerprint) of the
    /// metadata type the producer encodes (0 skips the check).
    ///
    /// # Errors
    ///
    /// [`SubmitStatus::UnknownSink`], [`SubmitStatus::Closed`] (recently
    /// closed, or after shutdown) or [`SubmitStatus::TypeMismatch`].
    pub fn open_native_producer(
        &self,
        sink: u64,
        fingerprint: u64,
    ) -> Result<NativeProducer, SubmitStatus> {
        self.frame_sinks.producer(sink, fingerprint)
    }

    /// Drop the registry entry of a sink whose consumer went away.
    pub(crate) fn forget_frame_sink(&self, sink: u64) {
        self.frame_sinks.remove(sink);
    }

    /// Whether frame sinks opened here can be fed by `plugin_id`'s host:
    /// `false` for plugins declared remote
    /// ([`declare_remote_plugin`](Self::declare_remote_plugin)), whose
    /// producers live in another process and cannot resolve a sink id.
    #[must_use]
    pub fn shares_native_frames(&self, plugin_id: &str) -> bool {
        !self.is_remote_plugin(plugin_id)
    }

    /// Track a stream served by a typed in-process shortcut like a
    /// hosted stream, so [`shutdown`](Self::shutdown) cancels it.
    pub(crate) fn begin_local_stream(&self) -> Result<(StreamId, CancelToken), IstmoError> {
        self.ensure_running()?;
        let id = self.next_id();
        let token = lock(&self.hosted)
            .begin(Origin::Local, CallId(id))
            .ok_or(IstmoError::RuntimeShutDown)?;
        Ok((StreamId(id), token))
    }

    /// Stop tracking a typed in-process stream.
    pub(crate) fn finish_local_stream(&self, stream_id: StreamId) {
        lock(&self.hosted).finish(Origin::Local, CallId(stream_id.get()));
    }

    /// Record that this runtime expects to call plugin `plugin_id`.
    ///
    /// Interacts with [`Runtime::set_enforce_declarations`] to reject
    /// calls to plugins that were never wired up.
    pub fn declare_plugin(&self, plugin_id: &'static str) {
        lock(&self.declared_plugins).insert(plugin_id);
    }

    /// Return `true` if `plugin_id` has been declared on this runtime.
    #[must_use]
    pub fn is_plugin_declared(&self, plugin_id: &str) -> bool {
        lock(&self.declared_plugins).contains(plugin_id)
    }

    /// Toggle strict declaration enforcement.
    ///
    /// When enabled, [`check_declared`](Self::check_declared) rejects
    /// calls to plugins that were not declared via
    /// [`declare_plugin`](Self::declare_plugin).
    pub fn set_enforce_declarations(&self, enforce: bool) {
        self.enforce_declarations.store(enforce, Ordering::Relaxed);
    }

    /// Mark `plugin_id` as living in a separate process.
    ///
    /// Frames targeted at a remote plugin are steered through the sink
    /// installed via [`install_remote_envelope_sink`](Self::install_remote_envelope_sink)
    /// instead of the outbound envelope channel.
    pub fn declare_remote_plugin(&self, plugin_id: impl Into<String>) {
        lock(&self.remote_plugins).insert(plugin_id.into());
    }

    /// Return `true` if `plugin_id` is routed through the remote sink.
    #[must_use]
    pub fn is_remote_plugin(&self, plugin_id: &str) -> bool {
        lock(&self.remote_plugins).contains(plugin_id)
    }

    /// Install (or replace) the sink that receives envelopes destined
    /// for a remote-process plugin.
    ///
    /// The sink is called from whatever thread produced the outbound
    /// frame; it is expected to enqueue the bytes onto the platform's
    /// IPC channel and return quickly.
    pub fn install_remote_envelope_sink(&self, sink: RemoteEnvelopeSink) {
        *lock(&self.remote_sink) = Some(sink);
    }

    /// Send a frame, blocking while the outbound channel is full (on a
    /// thread that may block; see the type's `# Threads`).
    fn send_outbound(&self, envelope: Envelope) -> Result<(), IstmoError> {
        let Some(envelope) = self.route_remote(envelope)? else {
            return Ok(());
        };
        self.push_outbound(envelope, Wait::Block).map(drop)
    }

    /// Send a frame without blocking: for drop, cancel and release
    /// paths, which must not stall when the transport stopped draining.
    fn try_send_outbound(&self, envelope: Envelope) -> Result<(), IstmoError> {
        let Some(envelope) = self.route_remote(envelope)? else {
            return Ok(());
        };
        self.push_outbound(envelope, Wait::Never).map(drop)
    }

    /// Put `envelope` on the outbound channel from a synchronous caller.
    /// `Ok(false)` when `wait` gave up because the call was cancelled
    /// first.
    ///
    /// On `wasm32` nothing drains the channel while this frame waits: a
    /// frame that cannot go now (no room, or frames queued ahead of it)
    /// queues behind the others for [`Wait::Never`], like an async send,
    /// and fails with [`IstmoError::OutboundFull`] for any other wait.
    fn push_outbound(&self, envelope: Envelope, wait: Wait<'_>) -> Result<bool, IstmoError> {
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        {
            pollster::block_on(self.push_outbound_async(envelope, wait.for_sync()))
        }
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        {
            self.push_outbound_now(envelope, wait)
        }
    }

    /// Count a frame as in flight before it can reach the channel. A
    /// transport may take it and report it delivered before a late
    /// increment, which would lose that decrement for good; and while it
    /// sat queued but uncounted, a direct call could overtake it. The
    /// count is given back unless the attempt is settled as sent, also
    /// when the attempt is dropped half way.
    fn count_outbound(&self) -> InFlight<'_> {
        self.outbound_backlog.fetch_add(1, Ordering::AcqRel);
        InFlight(Some(&*self.outbound_backlog))
    }

    /// Put `envelope` on the outbound channel. `Ok(false)` when `wait`
    /// gave up because the call was cancelled first.
    async fn push_outbound_async(
        &self,
        envelope: Envelope,
        wait: Wait<'_>,
    ) -> Result<bool, IstmoError> {
        let in_flight = self.count_outbound();
        if self.spill.is_active() {
            return self.push_behind_spill(envelope, wait, in_flight).await;
        }
        let sent = match wait {
            // Room on the channel goes first, shut down or not: frames
            // already decided (an error answer after shutdown) still
            // reach a transport that keeps draining.
            Wait::Block => match self.outbound.try_send(envelope) {
                Ok(()) => Ok(true),
                Err(TrySendError::Disconnected(_)) => Err(IstmoError::ChannelClosed),
                Err(TrySendError::Full(envelope)) => {
                    match send_unless_cancelled(&self.outbound, envelope, &self.stopped).await {
                        Delivery::Sent => Ok(true),
                        Delivery::Cancelled => Err(IstmoError::RuntimeShutDown),
                        Delivery::Disconnected => Err(IstmoError::ChannelClosed),
                    }
                }
            },
            Wait::UnlessCancelled(cancel) => {
                match send_unless_cancelled(&self.outbound, envelope, cancel).await {
                    Delivery::Sent => Ok(true),
                    Delivery::Cancelled => Ok(false),
                    Delivery::Disconnected => Err(IstmoError::ChannelClosed),
                }
            }
            Wait::Never => match self.outbound.try_send(envelope) {
                Ok(()) => Ok(true),
                Err(TrySendError::Disconnected(_)) => Err(IstmoError::ChannelClosed),
                Err(TrySendError::Full(envelope)) => {
                    return self.push_behind_spill(envelope, wait, in_flight).await;
                }
            },
        };
        in_flight.settle(sent)
    }

    /// Queue `envelope` behind the frames that could not wait for room
    /// (see `runtime/spill.rs`), which takes over its count. A sender that
    /// may wait still waits, for its own frame to leave the queue, unless
    /// `wait` gives up first.
    async fn push_behind_spill(
        &self,
        envelope: Envelope,
        wait: Wait<'_>,
        in_flight: InFlight<'_>,
    ) -> Result<bool, IstmoError> {
        in_flight.hand_over();
        let (stop, on_stop) = match wait {
            Wait::Block => (&self.stopped, Err(IstmoError::RuntimeShutDown)),
            Wait::UnlessCancelled(cancel) => (cancel, Ok(false)),
            Wait::Never => {
                self.spill.push(envelope, false);
                return Ok(true);
            }
        };
        let Some(mut ticket) = self.spill.push(envelope, true) else {
            return Ok(true);
        };
        match race(&mut ticket.done, stop.cancelled()).await {
            Either::Left(Ok(sent)) => sent,
            Either::Left(Err(_)) => Err(IstmoError::ChannelClosed),
            Either::Right(()) if self.spill.withdraw(&ticket) => on_stop,
            // Already taken by the queue's drain, which after shutdown
            // only tries for a short flush window: its verdict comes soon.
            Either::Right(()) if matches!(wait, Wait::Block) => {
                ticket.done.await.unwrap_or(Err(IstmoError::ChannelClosed))
            }
            // Taken by the drain, which waits for room regardless of this
            // call: it goes out (or fails) without this sender.
            Either::Right(()) => Ok(true),
        }
    }

    /// [`push_outbound`](Self::push_outbound) on `wasm32`, without
    /// waiting at all.
    #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
    fn push_outbound_now(&self, envelope: Envelope, wait: Wait<'_>) -> Result<bool, IstmoError> {
        let in_flight = self.count_outbound();
        // While frames are queued, room on the channel belongs to them:
        // taking it would let this frame overtake them.
        let envelope = if self.spill.is_active() {
            envelope
        } else {
            match self.outbound.try_send(envelope) {
                Ok(()) => return in_flight.settle(Ok(true)),
                Err(TrySendError::Disconnected(_)) => {
                    return in_flight.settle(Err(IstmoError::ChannelClosed));
                }
                Err(TrySendError::Full(envelope)) => envelope,
            }
        };
        if matches!(wait, Wait::Never) {
            in_flight.hand_over();
            self.spill.push(envelope, false);
            return Ok(true);
        }
        tracing::warn!(frame = ?envelope.frame, "outbound channel full; frame dropped");
        in_flight.settle(Err(IstmoError::OutboundFull))
    }

    /// Send a frame of a hosted call to whoever issued it, other than
    /// this runtime. `Ok(false)` when `wait` gave up because the call
    /// was cancelled first.
    async fn send_to_issuer(
        &self,
        origin: Origin,
        envelope: Envelope,
        wait: Wait<'_>,
    ) -> Result<bool, IstmoError> {
        let envelope = match origin {
            Origin::Remote => {
                let sink = lock(&self.remote_sink).clone();
                let Some(sink) = sink else {
                    // A bridge without a sink shuttles the outbound
                    // channel instead.
                    return self.push_outbound_async(envelope, wait).await;
                };
                sink(envelope.to_wire_bytes()?);
                return Ok(true);
            }
            Origin::Peer | Origin::Local => match self.route_remote(envelope)? {
                Some(envelope) => envelope,
                None => return Ok(true),
            },
        };
        self.push_outbound_async(envelope, wait).await
    }

    /// Hand `envelope` to the remote sink when it belongs to a remote
    /// call; otherwise give it back for the outbound channel.
    ///
    /// Frames are classified even while no sink is installed, so remote
    /// tracking entries still leave on their call's terminal frame.
    fn route_remote(&self, envelope: Envelope) -> Result<Option<Envelope>, IstmoError> {
        if !self.classify_and_track(&envelope.frame) {
            return Ok(Some(envelope));
        }
        let sink = lock(&self.remote_sink).clone();
        let Some(sink) = sink else {
            return Ok(Some(envelope));
        };
        sink(envelope.to_wire_bytes()?);
        Ok(None)
    }

    /// Decide whether an outbound frame goes to the remote sink, keeping
    /// [`RemoteCalls`] in step.
    fn classify_and_track(&self, frame: &Frame) -> bool {
        match frame {
            Frame::Call {
                call_id, plugin_id, ..
            } => self.track_outgoing(plugin_id, *call_id, OutgoingKind::Call),
            Frame::CreateInstance {
                call_id, plugin_id, ..
            } => self.track_outgoing(plugin_id, *call_id, OutgoingKind::CreateInstance),
            Frame::Notify { plugin_id, .. } => lock(&self.remote_plugins).contains(plugin_id),
            Frame::Respond { call_id, .. } => {
                lock(&self.remote_calls).incoming.remove(&call_id.get())
            }
            Frame::Event { stream_id, .. } => {
                lock(&self.remote_calls).incoming.contains(&stream_id.get())
            }
            Frame::StreamEnd { stream_id, .. } => {
                lock(&self.remote_calls).incoming.remove(&stream_id.get())
            }
            Frame::Cancel { call_id } => lock(&self.remote_calls)
                .outgoing
                .remove(&call_id.get())
                .is_some(),
            Frame::DestroyInstance { instance_id } => {
                lock(&self.remote_instances).remove(instance_id)
            }
            Frame::EarlyEvent { .. } | Frame::ReleaseNativeHandle { .. } => false,
        }
    }

    fn track_outgoing(&self, plugin_id: &str, call_id: CallId, kind: OutgoingKind) -> bool {
        if !lock(&self.remote_plugins).contains(plugin_id) {
            return false;
        }
        lock(&self.remote_calls)
            .outgoing
            .insert(call_id.get(), kind);
        true
    }

    /// Drop the tracking of an outgoing remote call once its terminal
    /// frame arrives; a remote `CreateInstance` response also marks the
    /// new instance remote.
    fn settle_outgoing_remote(&self, id: u64, result: Option<&CallResult>) {
        let kind = lock(&self.remote_calls).outgoing.remove(&id);
        if kind == Some(OutgoingKind::CreateInstance)
            && let Some(Ok(bytes)) = result
            && let Ok((instance_id, _)) = crate::codec::decode::<InstanceId>(bytes)
        {
            lock(&self.remote_instances).insert(instance_id);
        }
    }

    /// Record that a given [`InstanceId`] lives in a remote process.
    ///
    /// Instances created through a remote plugin's
    /// [`Frame::CreateInstance`] are recorded automatically; this covers
    /// instances learned about some other way.
    #[doc(hidden)]
    pub fn mark_instance_remote(&self, instance_id: InstanceId) {
        lock(&self.remote_instances).insert(instance_id);
    }

    /// Record that a given [`CallId`] originated on a remote process.
    ///
    /// For a bridge that hands remote calls to
    /// [`dispatch_inbound`](Self::dispatch_inbound): call this before
    /// injecting the call, so the eventual [`Frame::Respond`] (or the
    /// stream's events and end) is steered back through the sink.
    /// [`inject_wire_envelope`](Self::inject_wire_envelope) needs no
    /// marking, and keeps remote ids apart from the platform
    /// transport's, which this marking cannot.
    #[doc(hidden)]
    pub fn mark_call_remote(&self, call_id: CallId) {
        lock(&self.remote_calls).incoming.insert(call_id.get());
    }

    /// Verify that `plugin_id` has been declared on this runtime.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is enabled
    /// and the plugin is missing from the declaration set.
    pub fn check_declared(&self, plugin_id: &'static str) -> Result<(), IstmoError> {
        if !self.enforce_declarations.load(Ordering::Relaxed) {
            return Ok(());
        }
        if lock(&self.declared_plugins).contains(plugin_id) {
            return Ok(());
        }
        Err(IstmoError::PluginNotDeclared(plugin_id))
    }

    fn dispatch_hosted_call(self: &Arc<Self>, dispatcher: Arc<dyn Dispatch>, call: HostedCall) {
        self.dispatch_hosted_call_with(dispatcher, call, None);
    }

    /// Serve a hosted call. With `direct`, an answer ready before this
    /// returns is left in it for the synchronous caller instead of being
    /// sent; `false` when the call did not run inline, so no answer can
    /// land there.
    fn dispatch_hosted_call_with(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatch>,
        call: HostedCall,
        direct: Option<Arc<DirectReply>>,
    ) -> bool {
        if !dispatcher.admits(call.origin.into()) {
            self.refuse_origin(call.origin, call.call_id, dispatcher.plugin_id());
            return false;
        }
        let Some(cancel) = self.begin_hosted(call.origin, call.call_id) else {
            return false;
        };
        match dispatcher.method_mode(&call.method) {
            DispatchMode::Inline => {
                self.dispatch_hosted_call_inline(dispatcher, call, cancel, direct);
                true
            }
            DispatchMode::Isolated => {
                self.dispatch_hosted_call_blocking(dispatcher, call, cancel);
                false
            }
        }
    }

    /// Serve a call from the platform peer (Kotlin / Swift in this
    /// process) and hand its answer back when the host gives it before
    /// this returns: an [`Inline`](DispatchMode::Inline) host that
    /// completes without waiting. `None` means the answer goes out as a
    /// [`Frame::Respond`] once it exists, like for
    /// [`dispatch_inbound`](Self::dispatch_inbound) — a host that waits,
    /// a blocking host, a stream, a refused or unhosted call, a call the
    /// peer marked as coming from another process.
    ///
    /// The typed inbound wrapper behind the transports' synchronous call
    /// entries (Android `nativeCall`, iOS `istmo_ios_call`).
    pub fn serve_peer_call(
        self: &Arc<Self>,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    ) -> Option<CallResult> {
        let call = HostedCall {
            origin: Origin::Peer,
            call_id,
            instance_id,
            method,
            payload,
        };
        let Some(dispatcher) = self.host(plugin_id) else {
            self.reject_unhosted(Origin::Peer, call_id, plugin_id, "Call");
            return None;
        };
        // A bridge-forwarded call answers through the remote sink.
        if lock(&self.remote_calls).incoming.contains(&call_id.get()) {
            self.dispatch_hosted_call(dispatcher, call);
            return None;
        }
        let direct = Arc::new(DirectReply::default());
        if !self.dispatch_hosted_call_with(dispatcher, call, Some(Arc::clone(&direct))) {
            return None;
        }
        direct.close()
    }

    /// Open a stream for the platform peer that takes items by credit:
    /// the host's items go out only while the peer has credit left, and
    /// `window` is the credit it starts with. The peer grants more with
    /// [`grant_stream_credit`](Self::grant_stream_credit) as its consumer
    /// takes items, so its buffer never holds more than it granted and a
    /// fast producer waits instead of overflowing it (its own bounded
    /// channel fills and holds it back).
    ///
    /// Otherwise like a [`Frame::Call`] through
    /// [`dispatch_inbound`](Self::dispatch_inbound): events, the end and
    /// any error answer go out as frames. The typed inbound wrapper
    /// behind Android `nativeSubmitStream`.
    pub fn serve_peer_stream(
        self: &Arc<Self>,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
        window: u32,
    ) {
        let stream_id = StreamId(call_id.get());
        let (tx, rx) = flume::unbounded();
        // Unbounded and still connected: the send cannot fail.
        let _ = tx.send(window.max(1));
        lock(&self.peer_credits).insert(stream_id, CreditLane { tx, rx: Some(rx) });
        let Some(dispatcher) = self.host(plugin_id) else {
            self.forget_peer_credit(stream_id);
            self.reject_unhosted(Origin::Peer, call_id, plugin_id, "Call");
            return;
        };
        self.dispatch_hosted_call(
            dispatcher,
            HostedCall {
                origin: Origin::Peer,
                call_id,
                instance_id,
                method,
                payload,
            },
        );
        // A call that never started (refused for its origin, after
        // shutdown, no thread for it) has no pump to take its lane and
        // no answer path to drop it; one that finished already dropped
        // it.
        if !lock(&self.hosted).is_active(Origin::Peer, call_id) {
            self.forget_peer_credit(stream_id);
        }
    }

    /// Hand the consumer of `stream_id` an item from the platform peer
    /// without waiting: `Ok(false)` when its buffer is full and nothing
    /// was delivered.
    ///
    /// For a transport on a thread that must not block (see
    /// [`DeliveryScope::may_block`]): it offers the item first and moves
    /// the blocking [`dispatch_inbound`](Self::dispatch_inbound) of the
    /// same item to a thread that may only when the offer fails, so the
    /// common case costs one call.
    ///
    /// # Errors
    ///
    /// As [`dispatch_inbound`](Self::dispatch_inbound) for a
    /// [`Frame::Event`]: [`IstmoError::UnknownStreamId`],
    /// [`IstmoError::RoutingMismatch`], [`IstmoError::ChannelClosed`].
    pub fn offer_peer_event(
        &self,
        stream_id: StreamId,
        payload: Vec<u8>,
    ) -> Result<bool, IstmoError> {
        match self
            .routing
            .stream_sender(stream_id)?
            .try_send(StreamMessage::Event(payload))
        {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(_)) => Ok(false),
            Err(TrySendError::Disconnected(_)) => Err(IstmoError::ChannelClosed),
        }
    }

    /// End the stream `stream_id` from the platform peer without
    /// waiting: `Ok(false)` when the consumer's buffer is full and
    /// nothing was delivered; the stream stays open for a later
    /// [`dispatch_inbound`](Self::dispatch_inbound) of the same end.
    ///
    /// The end's [`offer_peer_event`](Self::offer_peer_event): a
    /// transport relaying a stream on a thread that must not block
    /// offers its end too, and moves it to a thread that may when the
    /// offer fails, behind any item it moved there before.
    ///
    /// # Errors
    ///
    /// As [`dispatch_inbound`](Self::dispatch_inbound) for a
    /// [`Frame::StreamEnd`]: [`IstmoError::UnknownStreamId`],
    /// [`IstmoError::RoutingMismatch`].
    pub fn offer_peer_stream_end(
        &self,
        stream_id: StreamId,
        reason: StreamEndReason,
    ) -> Result<bool, IstmoError> {
        let ended = self.routing.offer_stream_end(stream_id, reason)?;
        if ended {
            self.settle_outgoing_remote(stream_id.get(), None);
        }
        Ok(ended)
    }

    /// The peer's consumer of `stream_id` took `items` more: let that
    /// many more go out. Ignored for streams that ended or were not
    /// opened with [`serve_peer_stream`](Self::serve_peer_stream).
    pub fn grant_stream_credit(&self, stream_id: StreamId, items: u32) {
        if let Some(lane) = lock(&self.peer_credits).get(&stream_id) {
            // The pump may have dropped its end already: nothing to do.
            let _ = lane.tx.send(items);
        }
    }

    fn take_peer_credit(&self, stream_id: StreamId) -> Option<FlumeReceiver<u32>> {
        lock(&self.peer_credits).get_mut(&stream_id)?.rx.take()
    }

    fn forget_peer_credit(&self, stream_id: StreamId) {
        lock(&self.peer_credits).remove(&stream_id);
    }

    /// Serve the call on a blocking worker ([`DispatchMode::Isolated`]).
    fn dispatch_hosted_call_blocking(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatch>,
        call: HostedCall,
        cancel: CancelToken,
    ) {
        let runtime = Arc::clone(self);
        let HostedCall {
            origin,
            call_id,
            instance_id,
            method,
            payload,
        } = call;
        let refuse_cancel = cancel.clone();
        let spawned = exec::spawn_blocking(async move {
            // The dispatcher is shared, so a panic can leave its state
            // half-updated for later calls whether or not it is caught.
            // Catching it at least answers this call instead of leaving
            // the caller waiting forever; the generated hosts recover
            // poisoned locks as `sync::lock` does.
            let outcome = CatchUnwind::new(dispatcher.dispatch_from(
                origin.into(),
                instance_id,
                &method,
                &payload,
                cancel.clone(),
            ))
            .await;
            if let Ok(Ok(Outcome::StreamOpened(_))) = &outcome {
                // A stream pump waits for items most of its life: it
                // belongs on the async workers, not on a blocking thread.
                exec::spawn(async move {
                    runtime
                        .finish_hosted_call(origin, call_id, &cancel, outcome, None)
                        .await;
                });
                return;
            }
            runtime
                .finish_hosted_call(origin, call_id, &cancel, outcome, None)
                .await;
        });
        if let Err(err) = spawned {
            self.refuse_spawn(origin, call_id, &refuse_cancel, err);
        }
    }

    /// Serve the call on the current thread ([`DispatchMode::Inline`]):
    /// a host that completes without waiting is answered before this
    /// returns; one that waits continues on the async workers.
    fn dispatch_hosted_call_inline(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatch>,
        call: HostedCall,
        cancel: CancelToken,
        direct: Option<Arc<DirectReply>>,
    ) {
        let runtime = Arc::clone(self);
        let HostedCall {
            origin,
            call_id,
            instance_id,
            method,
            payload,
        } = call;
        exec::run_here(async move {
            // Same unwind reasoning as the blocking path.
            let outcome = CatchUnwind::new(dispatcher.dispatch_from(
                origin.into(),
                instance_id,
                &method,
                &payload,
                cancel.clone(),
            ))
            .await;
            if let Ok(Ok(Outcome::StreamOpened(_))) = &outcome {
                // The pump runs as long as the stream; leave the
                // delivering thread now rather than forward the items
                // already buffered on it.
                exec::yield_now().await;
            }
            runtime
                .finish_hosted_call(origin, call_id, &cancel, outcome, direct.as_deref())
                .await;
        });
    }

    /// Answer a hosted call whose task could not start.
    fn refuse_spawn(
        &self,
        origin: Origin,
        call_id: CallId,
        cancel: &CancelToken,
        err: std::io::Error,
    ) {
        tracing::error!(?err, ?call_id, "failed to spawn a host thread");
        let message = DispatchError::Spawn(err).to_string();
        exec::finish_now(self.respond(
            origin,
            call_id,
            Err(encode_dispatch_error_string(&message)),
            Wait::UnlessCancelled(cancel).for_sync(),
        ));
        lock(&self.hosted).finish(origin, call_id);
    }

    /// Start tracking a hosted call, or answer it with an error when the
    /// runtime shut down.
    fn begin_hosted(&self, origin: Origin, call_id: CallId) -> Option<CancelToken> {
        let cancel = if self.is_shut_down() {
            None
        } else {
            lock(&self.hosted).begin(origin, call_id)
        };
        if cancel.is_none() {
            exec::finish_now(self.respond(
                origin,
                call_id,
                Err(encode_dispatch_error_string(
                    &IstmoError::RuntimeShutDown.to_string(),
                )),
                Wait::Never,
            ));
        }
        cancel
    }

    async fn finish_hosted_call(
        &self,
        origin: Origin,
        call_id: CallId,
        cancel: &CancelToken,
        outcome: Result<Result<Outcome, DispatchError>, Box<dyn Any + Send>>,
        direct: Option<&DirectReply>,
    ) {
        let result = match outcome {
            Ok(Ok(Outcome::StreamOpened(receiver))) => {
                // The call stays active: the pump owns it until the
                // stream ends, so a cancel still reaches the producer.
                let stream_id = StreamId(call_id.get());
                let reason = self.pump_stream(origin, stream_id, cancel, &receiver).await;
                // Release the producer before the end goes out.
                drop(receiver);
                if origin == Origin::Peer {
                    self.forget_peer_credit(stream_id);
                }
                self.end_stream(origin, stream_id, reason, cancel).await;
                lock(&self.hosted).finish(origin, call_id);
                return;
            }
            Ok(Ok(Outcome::Ok(bytes))) => Ok(bytes),
            Ok(Ok(Outcome::DomainError(bytes))) => Err(bytes),
            Ok(Err(err)) => {
                tracing::error!(?err, ?call_id, "hosted dispatch failed");
                Err(encode_dispatch_error_string(&err.to_string()))
            }
            Err(_) => {
                tracing::error!(?call_id, "hosted dispatch panicked");
                Err(encode_dispatch_error_string("host panicked"))
            }
        };
        // A stream call answered like a unary one (refused, failed to
        // open) leaves no credit lane behind.
        if origin == Origin::Peer {
            self.forget_peer_credit(StreamId(call_id.get()));
        }
        // A synchronous caller still waiting takes the answer itself,
        // unless a frame queued before it (a notification the host sent)
        // is still on its way: the answer then follows it as a frame.
        let direct = direct.filter(|_| !self.frames_on_their_way());
        let result = match direct {
            Some(direct) => match direct.offer(result) {
                Ok(()) => {
                    lock(&self.hosted).finish(origin, call_id);
                    return;
                }
                Err(result) => result,
            },
            None => result,
        };
        // The call leaves the table only once its response is out, so a
        // cancel still frees a response waiting on a full channel.
        self.respond_unless_cancelled(origin, call_id, result, cancel)
            .await;
        lock(&self.hosted).finish(origin, call_id);
    }

    fn dispatch_hosted_create_instance(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatch>,
        origin: Origin,
        call_id: CallId,
        plugin_id: String,
        payload: Vec<u8>,
    ) {
        if !dispatcher.admits(origin.into()) {
            self.refuse_origin(origin, call_id, &plugin_id);
            return;
        }
        let Some(cancel) = self.begin_hosted(origin, call_id) else {
            return;
        };
        let runtime = Arc::clone(self);
        let task_cancel = cancel.clone();
        let mode = dispatcher.mode();
        let task = async move {
            // Same unwind reasoning as `dispatch_hosted_call`.
            let outcome =
                CatchUnwind::new(dispatcher.create_instance(&payload, task_cancel.clone())).await;
            let result = match outcome {
                Ok(Ok(Outcome::Ok(bytes))) => {
                    match crate::codec::decode::<InstanceId>(&bytes) {
                        Ok((instance_id, _)) if task_cancel.is_cancelled() => {
                            // Nobody will learn the id: reap the instance.
                            let reaped = catch_unwind(AssertUnwindSafe(|| {
                                dispatcher.destroy_instance(instance_id);
                            }));
                            if reaped.is_err() {
                                tracing::error!(?call_id, "hosted destroy_instance panicked");
                            }
                        }
                        Ok((instance_id, _)) => {
                            runtime.register_instance(instance_id, plugin_id);
                        }
                        Err(err) => tracing::error!(
                            ?err,
                            ?call_id,
                            "hosted create_instance returned undecodable InstanceId bytes",
                        ),
                    }
                    Ok(bytes)
                }
                Ok(Ok(Outcome::DomainError(bytes))) => Err(bytes),
                Ok(Ok(Outcome::StreamOpened(_))) => {
                    tracing::error!(?call_id, "hosted create_instance returned StreamOpened");
                    Err(encode_dispatch_error_string(
                        "create_instance returned StreamOpened",
                    ))
                }
                Ok(Err(err)) => {
                    tracing::error!(?err, ?call_id, "hosted create_instance failed");
                    Err(encode_dispatch_error_string(&err.to_string()))
                }
                Err(_) => {
                    tracing::error!(?call_id, "hosted create_instance panicked");
                    Err(encode_dispatch_error_string("host panicked"))
                }
            };
            runtime
                .respond_unless_cancelled(origin, call_id, result, &task_cancel)
                .await;
            lock(&runtime.hosted).finish(origin, call_id);
        };
        match mode {
            DispatchMode::Inline => {
                exec::run_here(task);
            }
            DispatchMode::Isolated => {
                if let Err(err) = exec::spawn_blocking(task) {
                    self.refuse_spawn(origin, call_id, &cancel, err);
                }
            }
        }
    }

    fn dispatch_hosted_destroy_instance(&self, origin: Origin, instance_id: InstanceId) {
        let Some(plugin_id) = self.routing.instance(instance_id) else {
            return;
        };
        let Some(dispatcher) = self.host(&plugin_id) else {
            self.routing.remove_instance(instance_id);
            return;
        };
        if !dispatcher.admits(origin.into()) {
            tracing::warn!(plugin_id = %plugin_id, ?origin, "refused DestroyInstance");
            return;
        }
        self.routing.remove_instance(instance_id);
        dispatcher.destroy_instance(instance_id);
    }

    /// Forward a hosted stream's items until the producer closes or fails
    /// it, or the consumer cancels.
    async fn pump_stream(
        &self,
        origin: Origin,
        stream_id: StreamId,
        cancel: &CancelToken,
        receiver: &FlumeReceiver<HostedStreamItem>,
    ) -> StreamEndReason {
        // A peer that opened the stream with a credit window takes at
        // most that many items ahead of its consumer: the pump holds the
        // next item until there is credit for it, so the producer's own
        // bounded channel is what fills and holds it back. The end and
        // errors need no credit.
        let mut credit = match origin {
            Origin::Peer => self.take_peer_credit(stream_id),
            Origin::Local | Origin::Remote => None,
        };
        let mut available: u64 = 0;
        loop {
            let step = race(receiver.recv_async(), cancel.cancelled()).await;
            // A producer that never stops sending keeps its receiver
            // ready, so the flag is what guarantees a cancel is seen.
            if cancel.is_cancelled() {
                return StreamEndReason::Cancelled;
            }
            match step {
                Either::Left(Ok(Ok(payload))) => {
                    if let Some(lane) = credit.take() {
                        match await_credit(&lane, &mut available, cancel).await {
                            Credit::Granted => {
                                available -= 1;
                                credit = Some(lane);
                            }
                            // The lane is gone (shutdown): stop gating.
                            Credit::Ungated => {}
                            Credit::Cancelled => return StreamEndReason::Cancelled,
                        }
                    }
                    if !self.emit_event(origin, stream_id, payload, cancel).await {
                        return StreamEndReason::Cancelled;
                    }
                }
                Either::Left(Ok(Err(error))) => return StreamEndReason::Error(error),
                Either::Left(Err(_)) => return StreamEndReason::Complete,
                Either::Right(()) => return StreamEndReason::Cancelled,
            }
        }
    }

    /// Forward one hosted stream item, waiting out a full consumer or
    /// outbound channel unless the stream is cancelled meanwhile.
    /// `false` once nobody can receive further items.
    async fn emit_event(
        &self,
        origin: Origin,
        stream_id: StreamId,
        payload: Vec<u8>,
        cancel: &CancelToken,
    ) -> bool {
        let delivered = match origin {
            Origin::Local => match self.routing.stream_sender(stream_id) {
                Ok(tx) => Ok(matches!(
                    send_unless_cancelled(&tx, StreamMessage::Event(payload), cancel).await,
                    Delivery::Sent
                )),
                Err(err) => Err(err),
            },
            Origin::Peer if self.event_to_peer_directly(stream_id, &payload) => Ok(true),
            Origin::Peer | Origin::Remote => {
                self.send_to_issuer(
                    origin,
                    Envelope::new(Frame::Event { stream_id, payload }),
                    Wait::UnlessCancelled(cancel),
                )
                .await
            }
        };
        delivered
            .inspect_err(|err| tracing::debug!(?err, ?stream_id, "hosted stream event undelivered"))
            .unwrap_or(false)
    }

    /// Hand a peer stream's item to the [`DirectPeer`] when nothing waits
    /// in the outbound channel ahead of it; `false`: it goes through the
    /// channel.
    fn event_to_peer_directly(&self, stream_id: StreamId, payload: &[u8]) -> bool {
        let Some(peer) = self.direct_peer.get() else {
            return false;
        };
        if self.outbound_backlog.load(Ordering::Acquire) != 0
            || lock(&self.remote_calls).incoming.contains(&stream_id.get())
        {
            return false;
        }
        peer.event(stream_id, payload)
    }

    /// Send a hosted stream's end, behind its items. A full local
    /// consumer is awaited like for an item, so the async worker serves
    /// other tasks meanwhile. Once the stream is cancelled the end is
    /// best effort and never waits: its issuer left, or the runtime shut
    /// down and the transport may have stopped draining.
    async fn end_stream(
        &self,
        origin: Origin,
        stream_id: StreamId,
        reason: StreamEndReason,
        cancel: &CancelToken,
    ) {
        let delivered = match origin {
            Origin::Local => match self.routing.take_stream_sender(stream_id) {
                Ok(tx) => {
                    let end = StreamMessage::End(reason);
                    Ok(if cancel.is_cancelled() {
                        tx.try_send(end).is_ok()
                    } else {
                        matches!(
                            send_unless_cancelled(&tx, end, cancel).await,
                            Delivery::Sent
                        )
                    })
                }
                Err(err) => Err(err),
            },
            Origin::Peer | Origin::Remote => {
                let wait = if cancel.is_cancelled() {
                    Wait::Never
                } else {
                    Wait::UnlessCancelled(cancel)
                };
                self.send_to_issuer(
                    origin,
                    Envelope::new(Frame::StreamEnd { stream_id, reason }),
                    wait,
                )
                .await
            }
        };
        if let Err(err) = delivered {
            tracing::debug!(?err, ?stream_id, "hosted stream end undelivered");
        }
    }

    /// Deliver a finished hosted call's result, unless the issuer
    /// cancelled it: a cancelled call from another runtime is answered
    /// by nobody.
    async fn respond_unless_cancelled(
        &self,
        origin: Origin,
        call_id: CallId,
        result: CallResult,
        cancel: &CancelToken,
    ) {
        // A local caller that left already removed its registration, so
        // completing it anyway is a harmless no-op.
        if origin != Origin::Local && cancel.is_cancelled() {
            tracing::debug!(?call_id, "hosted call cancelled; discarding response");
            self.forget_incoming_remote(origin, call_id);
            return;
        }
        self.respond(origin, call_id, result, Wait::UnlessCancelled(cancel))
            .await;
    }

    /// Deliver a hosted call's result to whoever issued it.
    async fn respond(&self, origin: Origin, call_id: CallId, result: CallResult, wait: Wait<'_>) {
        if origin == Origin::Local {
            if let Err(err) = self.routing.complete(call_id, result) {
                tracing::debug!(?err, ?call_id, "hosted call finished after its caller left");
            }
            return;
        }
        let envelope = Envelope::new(Frame::Respond { call_id, result });
        match self.send_to_issuer(origin, envelope, wait).await {
            Ok(true) => {}
            Ok(false) => {
                tracing::debug!(?call_id, "hosted call cancelled while its response waited");
                self.forget_incoming_remote(origin, call_id);
            }
            Err(err) => tracing::warn!(?err, ?call_id, "failed to send Respond frame"),
        }
    }

    /// A peer call through the remote sink that ends without a terminal
    /// frame must still leave the remote tracking.
    fn forget_incoming_remote(&self, origin: Origin, call_id: CallId) {
        if origin == Origin::Peer {
            lock(&self.remote_calls).incoming.remove(&call_id.get());
        }
    }

    fn publish_early(&self, channel: &str, kind: &EarlyEventKind, payload: Vec<u8>) {
        match kind {
            EarlyEventKind::Latest => self.publish_early_latest(channel, payload),
            EarlyEventKind::Queue { capacity } => {
                self.publish_early_queue(channel, *capacity as usize, payload);
            }
        }
    }

    /// Publish a value on an early-event channel with
    /// [`EarlyEventKind::Latest`] semantics.
    ///
    /// Only the most recent value is retained; late subscribers see it
    /// on first attach.
    pub fn publish_early_latest(&self, channel: &str, payload: Vec<u8>) {
        self.early_events.latest_slot(channel).publish(payload);
    }

    /// Publish onto an early-event channel with FIFO retention.
    ///
    /// Retains up to `capacity` values (at most
    /// [`MAX_EARLY_QUEUE_CAPACITY`](crate::early_events::MAX_EARLY_QUEUE_CAPACITY));
    /// overflow drops the oldest entry. Late subscribers receive the
    /// buffered items in publish order.
    pub fn publish_early_queue(&self, channel: &str, capacity: usize, payload: Vec<u8>) {
        self.early_events.queue(channel, capacity).publish(payload);
    }

    /// Stop accepting work and drop every piece of in-flight state.
    ///
    /// Afterwards [`call`](Self::call), [`stream`](Self::stream),
    /// [`create_instance`](Self::create_instance) and
    /// [`notify`](Self::notify) return [`IstmoError::RuntimeShutDown`],
    /// and inbound calls are answered with an error. Pending handles
    /// resolve with [`IstmoError::ChannelClosed`], hosted calls and
    /// streams have their [`CancelToken`]s tripped, every frame sink
    /// closes (queued frames are released), and the remote and
    /// instance tables are cleared. The outbound channel itself stays
    /// open until the runtime is dropped, so frames already queued still
    /// reach a transport that keeps draining; frames queued behind a
    /// full channel (see the type's `# Threads`) get half a second more to
    /// go out, then are dropped.
    ///
    /// Host threads still running finish on their own: their responses
    /// to other runtimes are discarded, and their streams stop at the
    /// next item, so none of them waits on a transport that stopped
    /// draining.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.stopped.cancel();
        self.spill.stop();
        let count = self.routing.clear();
        lock(&self.hosted).cancel_all();
        lock(&self.typed_hosts).clear();
        self.has_typed_hosts.store(false, Ordering::Release);
        *lock(&self.remote_calls) = RemoteCalls::default();
        lock(&self.remote_instances).clear();
        self.frame_sinks.close_all();
        lock(&self.peer_credits).clear();
        if count > 0 {
            tracing::info!(count, "istmo runtime shutdown cancelled pending operations");
        }
    }
}

impl Drop for Runtime {
    /// A runtime dropped without [`shutdown`](Self::shutdown) stops its
    /// outbound queue the same way: a drain lingering for more frames
    /// would otherwise keep the outbound channel open for up to a
    /// second, and frames still queued get the same flush window.
    fn drop(&mut self) {
        self.stopped.cancel();
        self.spill.stop();
    }
}

/// A frame counted in [`Runtime::outbound_backlog`] while it is being
/// sent; see [`Runtime::count_outbound`].
struct InFlight<'a>(Option<&'a AtomicUsize>);

impl InFlight<'_> {
    /// Keep the count when `sent` put the frame on the channel.
    fn settle(mut self, sent: Result<bool, IstmoError>) -> Result<bool, IstmoError> {
        if matches!(sent, Ok(true)) {
            self.0 = None;
        }
        sent
    }

    /// The frame was queued behind the channel; the queue owns the count
    /// from now on.
    fn hand_over(mut self) {
        self.0 = None;
    }
}

impl Drop for InFlight<'_> {
    fn drop(&mut self) {
        if let Some(count) = self.0 {
            count.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// How long a send for a hosted call may wait on a full channel.
#[derive(Debug, Clone, Copy)]
enum Wait<'a> {
    /// Until there is room, or until the runtime shuts down.
    Block,
    /// Until there is room, or until the call is cancelled.
    UnlessCancelled(&'a CancelToken),
    /// Not at all: a full channel queues the frame behind it, in order,
    /// and the send returns at once. Nothing is dropped that way: not a
    /// cancel, not a release.
    Never,
}

impl Wait<'_> {
    /// The wait a synchronous send on this thread may afford: none that
    /// could block where the runtime must not
    /// ([`DeliveryScope`], an async worker).
    #[allow(
        clippy::missing_const_for_fn,
        reason = "could be const only on wasm32, where may_block is"
    )]
    fn for_sync(self) -> Self {
        match self {
            Self::Block | Self::UnlessCancelled(_) if !exec::may_block() => Self::Never,
            other => other,
        }
    }
}

/// Run a notification in the background; nothing is sent back.
fn dispatch_notify(
    dispatcher: Arc<dyn Dispatch>,
    origin: Origin,
    instance_id: Option<InstanceId>,
    method: String,
    payload: Vec<u8>,
) {
    if !dispatcher.admits(origin.into()) {
        tracing::warn!(plugin_id = dispatcher.plugin_id(), ?origin, method = %method, "refused Notify");
        return;
    }
    let mode = dispatcher.method_mode(&method);
    let task = async move {
        // Same unwind reasoning as `Runtime::dispatch_hosted_call`.
        let outcome = CatchUnwind::new(dispatcher.dispatch_from(
            origin.into(),
            instance_id,
            &method,
            &payload,
            CancelToken::new(),
        ))
        .await;
        match outcome {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => tracing::warn!(?err, method = %method, "notify dispatch failed"),
            Err(_) => tracing::error!(method = %method, "notify dispatch panicked"),
        }
    };
    match mode {
        DispatchMode::Inline => {
            exec::run_here(task);
        }
        DispatchMode::Isolated => {
            if let Err(err) = exec::spawn_blocking(task) {
                tracing::error!(?err, "no blocking worker for a notification; dropped");
            }
        }
    }
}

/// Host side of a `#[istmo::stream]` method: encode the
/// implementation's typed items in the background and hand the runtime
/// the encoded stream. Called by `#[istmo::plugin]`-generated code.
///
/// `encode` returns the item's bytes, or the `Err` chunk that ends the
/// stream (a domain error, or an encode failure as a string).
///
/// The encoder is a task on the runtime's async workers.
///
/// # Errors
///
/// None today; the signature keeps room for an encoder that cannot
/// start.
#[doc(hidden)]
pub fn encode_hosted_stream<T: Send + 'static>(
    method: &'static str,
    source: FlumeReceiver<T>,
    capacity: usize,
    encode: impl Fn(T) -> HostedStreamItem + Send + 'static,
) -> Result<Outcome, DispatchError> {
    let _ = method;
    let (tx, rx) = bounded::<HostedStreamItem>(capacity);
    exec::spawn(async move {
        while let Ok(item) = source.recv_async().await {
            let chunk = encode(item);
            let ends = chunk.is_err();
            if tx.send_async(chunk).await.is_err() || ends {
                break;
            }
        }
    });
    Ok(Outcome::StreamOpened(rx))
}

/// What [`await_credit`] found.
enum Credit {
    /// At least one item may go out (`available > 0`).
    Granted,
    /// The credit lane closed: send without credit from now on.
    Ungated,
    /// The stream was cancelled while waiting.
    Cancelled,
}

/// Collect credit granted on `lane` into `available`, waiting while
/// there is none.
async fn await_credit(
    lane: &FlumeReceiver<u32>,
    available: &mut u64,
    cancel: &CancelToken,
) -> Credit {
    loop {
        while let Ok(granted) = lane.try_recv() {
            *available += u64::from(granted);
        }
        if *available > 0 {
            return Credit::Granted;
        }
        match race(lane.recv_async(), cancel.cancelled()).await {
            Either::Left(Ok(granted)) => *available += u64::from(granted),
            Either::Left(Err(_)) => return Credit::Ungated,
            Either::Right(()) => return Credit::Cancelled,
        }
    }
}

/// Wait for the next item of `rx` unless `cancel` trips first; `None`
/// when it did. The item wins a tie.
pub(crate) async fn recv_unless_cancelled<T>(
    rx: &FlumeReceiver<T>,
    cancel: &CancelToken,
) -> Option<Result<T, flume::RecvError>> {
    match race(rx.recv_async(), cancel.cancelled()).await {
        Either::Left(received) => Some(received),
        Either::Right(()) => None,
    }
}

fn encode_dispatch_error_string(message: &str) -> Vec<u8> {
    crate::codec::encode(&message.to_owned()).unwrap_or_default()
}

/// Await `fut`, catching a panic raised during its polling and returning
/// a wire-encoded `"host panicked"` string in its place.
///
/// Used by the typed in-process shortcut generated by
/// [`#[istmo::plugin]`](../../istmo_macros/attr.plugin.html): the typed
/// fast path bypasses the dispatcher's own panic catch, so the generated
/// client funnels the call through this helper to preserve the same
/// panic-to-wire semantics the wire path offers.
#[doc(hidden)]
pub async fn typed_catch_unwind<F, T>(fut: F) -> Result<T, Vec<u8>>
where
    F: Future<Output = T>,
{
    CatchUnwind::new(fut).await.map_err(|_| {
        tracing::error!("typed-shortcut host panicked");
        encode_dispatch_error_string("host panicked")
    })
}

/// The typed shortcut of a unary method taking a [`CancelToken`]: like
/// [`typed_catch_unwind`], with a token that trips when the caller drops
/// the call before it finished or the runtime shuts down, as on the
/// wire path.
///
/// A host that observes its token usually still has work to do (stop a
/// sensor, release a claim). On the wire path the runtime keeps polling
/// it after the caller left; here the caller's own task polled it, so a
/// call dropped early hands the host future to the runtime's async
/// workers, which run it to the end.
#[doc(hidden)]
pub async fn typed_call_cancellable<F, T>(
    runtime: &Arc<Runtime>,
    open: impl FnOnce(CancelToken) -> F,
) -> Result<T, IstmoError>
where
    F: Future<Output = T> + Send + 'static,
    T: Send + 'static,
{
    /// The host future until it finished; cancels and hands it off when
    /// dropped before that.
    struct Pending<F: Future + Send + 'static>
    where
        F::Output: Send,
    {
        runtime: Weak<Runtime>,
        id: StreamId,
        cancel: CancelToken,
        future: Option<CatchUnwind<F>>,
    }
    impl<F: Future + Send + 'static> Drop for Pending<F>
    where
        F::Output: Send,
    {
        fn drop(&mut self) {
            let finish = {
                let (runtime, id) = (self.runtime.clone(), self.id);
                move || {
                    if let Some(runtime) = runtime.upgrade() {
                        runtime.finish_local_stream(id);
                    }
                }
            };
            let Some(future) = self.future.take() else {
                finish();
                return;
            };
            self.cancel.cancel();
            exec::spawn(async move {
                if future.await.is_err() {
                    tracing::error!("typed-shortcut host panicked after its call was dropped");
                }
                finish();
            });
        }
    }
    let (id, cancel) = runtime.begin_local_stream()?;
    let mut pending = Pending {
        runtime: Arc::downgrade(runtime),
        id,
        cancel: cancel.clone(),
        future: Some(CatchUnwind::new(open(cancel))),
    };
    let outcome = std::future::poll_fn(|cx| {
        pending
            .future
            .as_mut()
            .map_or(Poll::Pending, |future| Pin::new(future).poll(cx))
    })
    .await;
    pending.future = None;
    outcome.map_err(|_| {
        tracing::error!("typed-shortcut host panicked");
        IstmoError::PluginError {
            bytes: encode_dispatch_error_string("host panicked"),
        }
    })
}

fn build(config: RuntimeConfig) -> RuntimeInit {
    let (outbound_tx, outbound_rx) = bounded(config.outbound_capacity);
    let outbound_backlog = Arc::new(AtomicUsize::new(0));
    let stopped = CancelToken::new();
    let spill = Arc::new(spill::Spill::new(
        outbound_tx.clone(),
        Arc::clone(&outbound_backlog),
        stopped.clone(),
    ));
    let runtime = Arc::new(Runtime {
        outbound: outbound_tx,
        routing: RoutingTables::default(),
        early_events: Arc::new(EarlyEventStore::new()),
        main_thread: config.main_thread,
        next_id: AtomicU64::new(1),
        next_native_handle: AtomicU64::new(NativeHandleId::RUST_ALLOCATED_BASE),
        shutting_down: AtomicBool::new(false),
        stopped,
        declared_plugins: Mutex::new(HashSet::new()),
        enforce_declarations: AtomicBool::new(false),
        hosts: Mutex::new(HashMap::new()),
        typed_hosts: Mutex::new(HashMap::new()),
        has_typed_hosts: AtomicBool::new(false),
        hosted: Mutex::new(HostedCalls::default()),
        remote_plugins: Mutex::new(HashSet::new()),
        remote_calls: Mutex::new(RemoteCalls::default()),
        remote_instances: Mutex::new(HashSet::new()),
        remote_sink: Mutex::new(None),
        native_handle_release_hooks: Mutex::new(Vec::new()),
        frame_sinks: FrameSinks::default(),
        direct_peer: OnceLock::new(),
        outbound_backlog,
        tracks_deliveries: AtomicBool::new(false),
        spill,
        peer_credits: Mutex::new(HashMap::new()),
    });
    RuntimeInit {
        runtime,
        outbound: outbound_rx,
    }
}

/// Handle to an in-flight unary call.
///
/// Returned by [`Runtime::call`] and [`Runtime::create_instance`]. The
/// handle implements [`Future`] so it can be `.await`ed; dropping it
/// before the response arrives cancels the call — by tripping the
/// hosted dispatch's [`CancelToken`] when a host on this runtime serves
/// it, or by sending [`Frame::Cancel`] to the peer otherwise.
#[derive(Debug)]
pub struct CallHandle {
    call_id: CallId,
    receiver: Option<oneshot::Receiver<CallResult>>,
    runtime: Weak<Runtime>,
    /// Served by a host registered on this runtime rather than by the
    /// peer — decides where a drop-cancel is delivered.
    hosted: bool,
}

impl CallHandle {
    /// The [`CallId`] this handle is waiting on.
    #[must_use]
    pub const fn call_id(&self) -> CallId {
        self.call_id
    }

    /// Synchronously block the current thread until the response
    /// arrives.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] if the response channel is closed
    /// before a value is delivered (usually a shutting-down runtime).
    pub fn recv_blocking(mut self) -> Result<CallResult, IstmoError> {
        static WAIT: crate::spin::SpinWait = crate::spin::SpinWait::new();
        let rx = self.receiver.take().ok_or(IstmoError::ChannelClosed)?;
        // A quick answer is caught without parking.
        let quick = WAIT.spin(|| match rx.try_recv() {
            Ok(result) => Some(Ok(result)),
            Err(oneshot::TryRecvError::Disconnected) => Some(Err(IstmoError::ChannelClosed)),
            Err(oneshot::TryRecvError::Empty) => None,
        });
        quick.unwrap_or_else(|| rx.recv().map_err(|_| IstmoError::ChannelClosed))
    }
}

impl Future for CallHandle {
    type Output = Result<CallResult, IstmoError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        let Some(rx) = this.receiver.as_mut() else {
            return Poll::Ready(Err(IstmoError::ChannelClosed));
        };
        match Pin::new(rx).poll(cx) {
            Poll::Ready(Ok(result)) => {
                this.receiver = None;
                Poll::Ready(Ok(result))
            }
            Poll::Ready(Err(_)) => {
                this.receiver = None;
                Poll::Ready(Err(IstmoError::ChannelClosed))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for CallHandle {
    fn drop(&mut self) {
        if self.receiver.is_none() {
            return;
        }
        let Some(rt) = self.runtime.upgrade() else {
            return;
        };
        if self.hosted {
            rt.cancel_hosted_local_call(self.call_id);
        } else if let Err(err) = rt.cancel_call(self.call_id) {
            tracing::debug!(?err, call_id = ?self.call_id, "drop-cancel not sent");
        }
    }
}

/// Handle to a live event stream.
///
/// Returned by [`Runtime::stream`]. Items are delivered in publish
/// order; dropping the handle before the stream ends cancels it — by
/// tripping the hosted producer's [`CancelToken`] when a host on this
/// runtime serves it, or by sending [`Frame::Cancel`] to the peer
/// otherwise. All three receive shapes (blocking, async, non-blocking
/// peek) are available.
#[derive(Debug)]
pub struct StreamHandle {
    stream_id: StreamId,
    receiver: FlumeReceiver<StreamMessage>,
    runtime: Weak<Runtime>,
    /// Served by a host registered on this runtime rather than by the
    /// peer — decides where a drop-cancel is delivered.
    hosted: bool,
    /// Set once [`StreamMessage::End`] was handed out; dropping an ended
    /// stream cancels nothing.
    ended: AtomicBool,
}

impl StreamHandle {
    /// The [`StreamId`] backing this handle.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    fn observe(&self, message: StreamMessage) -> StreamMessage {
        if matches!(message, StreamMessage::End(_)) {
            self.ended.store(true, Ordering::Relaxed);
        }
        message
    }

    /// Block the current thread until the next stream message arrives.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] if the stream's channel closes
    /// without delivering another value.
    pub fn recv(&self) -> Result<StreamMessage, IstmoError> {
        static WAIT: crate::spin::SpinWait = crate::spin::SpinWait::new();
        let quick = WAIT.spin(|| match self.receiver.try_recv() {
            Ok(message) => Some(Ok(message)),
            Err(flume::TryRecvError::Disconnected) => Some(Err(IstmoError::ChannelClosed)),
            Err(flume::TryRecvError::Empty) => None,
        });
        quick
            .unwrap_or_else(|| self.receiver.recv().map_err(|_| IstmoError::ChannelClosed))
            .map(|message| self.observe(message))
    }

    /// Async variant of [`recv`](Self::recv).
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] on channel disconnection.
    pub async fn recv_async(&self) -> Result<StreamMessage, IstmoError> {
        self.receiver
            .recv_async()
            .await
            .map(|message| self.observe(message))
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Non-blocking peek. Returns `None` if no message is buffered.
    #[must_use]
    pub fn try_recv(&self) -> Option<StreamMessage> {
        self.receiver
            .try_recv()
            .ok()
            .map(|message| self.observe(message))
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if self.ended.load(Ordering::Relaxed) {
            return;
        }
        let Some(rt) = self.runtime.upgrade() else {
            return;
        };
        if self.hosted {
            rt.cancel_hosted_local_call(CallId(self.stream_id.get()));
        } else if let Err(err) = rt.cancel_stream(self.stream_id) {
            tracing::debug!(?err, stream_id = ?self.stream_id, "drop-cancel not sent");
        }
    }
}

/// A dispatcher registered with [`Runtime::register_host_wire_only`]:
/// served like the one it wraps, always on a blocking worker and never
/// through a typed shortcut.
struct WireOnly<D>(D);

impl<D: Dispatch> Dispatch for WireOnly<D> {
    fn plugin_id(&self) -> &'static str {
        self.0.plugin_id()
    }

    fn mode(&self) -> DispatchMode {
        DispatchMode::Isolated
    }

    fn method_mode(&self, _method: &str) -> DispatchMode {
        DispatchMode::Isolated
    }

    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        self.0.runtime_attached(runtime);
    }

    fn admits(&self, origin: CallOrigin) -> bool {
        self.0.admits(origin)
    }

    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> crate::dispatch::DispatchFuture<'a> {
        self.0.dispatch(instance_id, method, payload, cancel)
    }

    fn dispatch_from<'a>(
        &'a self,
        origin: CallOrigin,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> crate::dispatch::DispatchFuture<'a> {
        self.0
            .dispatch_from(origin, instance_id, method, payload, cancel)
    }

    fn create_instance<'a>(
        &'a self,
        config_payload: &'a [u8],
        cancel: CancelToken,
    ) -> crate::dispatch::DispatchFuture<'a> {
        self.0.create_instance(config_payload, cancel)
    }

    fn destroy_instance(&self, instance_id: InstanceId) {
        self.0.destroy_instance(instance_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stream host that refuses the platform peer.
    struct PeerRefused;

    impl Dispatch for PeerRefused {
        fn plugin_id(&self) -> &'static str {
            "test.peer_refused"
        }

        fn admits(&self, origin: CallOrigin) -> bool {
            origin != CallOrigin::Peer
        }

        fn dispatch<'a>(
            &'a self,
            _instance_id: Option<InstanceId>,
            _method: &'a str,
            _payload: &'a [u8],
            _cancel: CancelToken,
        ) -> crate::dispatch::DispatchFuture<'a> {
            Box::pin(async { Ok(Outcome::StreamOpened(flume::bounded(1).1)) })
        }
    }

    #[test]
    fn streams_that_never_start_leave_no_credit_lane() {
        let init = Runtime::mock();
        let rt = &init.runtime;
        rt.register_host(PeerRefused);
        rt.serve_peer_stream(
            CallId(1),
            "test.peer_refused",
            None,
            "s".into(),
            Vec::new(),
            4,
        );
        assert!(lock(&rt.peer_credits).is_empty(), "refused for its origin");
        rt.shutdown();
        rt.serve_peer_stream(
            CallId(2),
            "test.peer_refused",
            None,
            "s".into(),
            Vec::new(),
            4,
        );
        assert!(lock(&rt.peer_credits).is_empty(), "after shutdown");
    }

    #[test]
    fn early_cancels_are_bounded_and_evict_the_oldest() {
        let mut hosted = HostedCalls::default();
        let ids = u64::try_from(EARLY_CANCEL_CAPACITY).unwrap() + 10;
        for id in 0..ids {
            hosted.cancel(Origin::Peer, CallId(id));
        }
        assert_eq!(hosted.early_cancels.len(), EARLY_CANCEL_CAPACITY);
        assert_eq!(hosted.early_order.len(), EARLY_CANCEL_CAPACITY);
        assert!(
            !hosted
                .begin(Origin::Peer, CallId(0))
                .expect("open")
                .is_cancelled()
        );
        assert!(
            hosted
                .begin(Origin::Peer, CallId(ids - 1))
                .expect("open")
                .is_cancelled()
        );
    }

    #[test]
    fn cancels_of_running_or_local_calls_are_not_remembered() {
        let mut hosted = HostedCalls::default();
        let token = hosted.begin(Origin::Peer, CallId(1)).expect("open");
        hosted.cancel(Origin::Peer, CallId(1));
        assert!(token.is_cancelled());
        hosted.finish(Origin::Peer, CallId(1));
        hosted.cancel(Origin::Local, CallId(2));
        assert!(hosted.active.is_empty());
        assert!(hosted.early_cancels.is_empty());
    }

    #[test]
    fn local_and_peer_calls_with_the_same_id_cancel_separately() {
        let mut hosted = HostedCalls::default();
        let local = hosted.begin(Origin::Local, CallId(1)).expect("open");
        let peer = hosted.begin(Origin::Peer, CallId(1)).expect("open");
        hosted.cancel(Origin::Peer, CallId(1));
        assert!(peer.is_cancelled());
        assert!(!local.is_cancelled());
    }

    type SentFrames = Arc<Mutex<Vec<Vec<u8>>>>;

    fn remote_runtime() -> (Arc<Runtime>, SentFrames) {
        let rt = Runtime::mock().runtime;
        rt.declare_remote_plugin("remote.p");
        let sent = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&sent);
        rt.install_remote_envelope_sink(Arc::new(move |bytes| lock(&sink).push(bytes)));
        (rt, sent)
    }

    #[test]
    fn outgoing_remote_calls_leave_tracking_on_their_response() {
        let (rt, sent) = remote_runtime();
        let handle = rt.call("remote.p", None, "m", Vec::new()).expect("call");
        assert_eq!(lock(&sent).len(), 1);
        assert_eq!(lock(&rt.remote_calls).outgoing.len(), 1);

        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: handle.call_id(),
            result: Ok(Vec::new()),
        }))
        .expect("respond");
        assert!(lock(&rt.remote_calls).outgoing.is_empty());
        drop(handle);
        assert_eq!(lock(&sent).len(), 1, "an answered call sends no Cancel");
    }

    #[test]
    fn outgoing_remote_streams_leave_tracking_on_their_end() {
        let (rt, _sent) = remote_runtime();
        let handle = rt
            .stream("remote.p", None, "m", Vec::new(), 0)
            .expect("stream");
        rt.dispatch_inbound(Envelope::new(Frame::StreamEnd {
            stream_id: handle.stream_id(),
            reason: StreamEndReason::Complete,
        }))
        .expect("end");
        assert!(lock(&rt.remote_calls).outgoing.is_empty());
    }

    #[test]
    fn remote_create_instance_marks_the_new_instance_remote() {
        let (rt, sent) = remote_runtime();
        let handle = rt.create_instance("remote.p", Vec::new()).expect("create");
        let instance_id = InstanceId(9);
        rt.dispatch_inbound(Envelope::new(Frame::Respond {
            call_id: handle.call_id(),
            result: Ok(crate::codec::encode(&instance_id).expect("encode")),
        }))
        .expect("respond");
        assert!(lock(&rt.remote_calls).outgoing.is_empty());

        rt.destroy_instance(instance_id).expect("destroy");
        let last = lock(&sent).last().cloned().expect("sink frame");
        assert!(matches!(
            Envelope::from_wire_bytes(&last).expect("decode").frame,
            Frame::DestroyInstance { .. }
        ));
        assert!(lock(&rt.remote_instances).is_empty());
    }

    #[test]
    fn incoming_remote_calls_leave_tracking_when_cancelled_before_responding() {
        let (rt, _sent) = remote_runtime();
        rt.mark_call_remote(CallId(5));
        rt.forget_incoming_remote(Origin::Peer, CallId(5));
        assert!(lock(&rt.remote_calls).incoming.is_empty());
    }

    #[test]
    fn shutdown_clears_in_flight_state() {
        let (rt, _sent) = remote_runtime();
        let _call = rt.call("remote.p", None, "m", Vec::new()).expect("call");
        rt.mark_call_remote(CallId(77));
        rt.mark_instance_remote(InstanceId(3));
        rt.register_instance(InstanceId(4), "local.p");
        rt.dispatch_inbound(Envelope::new(Frame::Cancel { call_id: CallId(8) }))
            .expect("cancel");

        rt.shutdown();
        assert!(lock(&rt.remote_calls).outgoing.is_empty());
        assert!(lock(&rt.remote_calls).incoming.is_empty());
        assert!(lock(&rt.remote_instances).is_empty());
        assert!(lock(&rt.hosted).early_cancels.is_empty());
        assert!(rt.instance_plugin_id(InstanceId(4)).is_none());
    }
}
