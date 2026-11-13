use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, TrySendError, bounded};

use crate::dispatch::{CancelToken, Dispatch, DispatchError, HostedStreamItem, Outcome, Plugin};
use crate::early_events::EarlyEventStore;
use crate::error::IstmoError;
use crate::main_thread::{InlineMainThread, MainThread};
use crate::protocol::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId, PROTOCOL_VERSION,
    StreamEndReason, StreamId,
};
use crate::routing::{CallResult, RoutingTables, StreamMessage};
use crate::sync::lock;

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
/// Every hosted call, notification and stream runs on its own named OS
/// thread. Hosts may block for as long as they like and may call back
/// into the same runtime while serving a call; a fixed-size pool would
/// deadlock once every worker waits on a call queued behind it.
///
/// A host thread that would wait on a full channel (the outbound
/// envelope channel, or a local consumer's bounded stream) gives up as
/// soon as its call is cancelled, so neither a consumer that stopped
/// reading nor a transport that stopped draining after
/// [`shutdown`](Self::shutdown) pins it forever.
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

    hosted: Mutex<HostedCalls>,

    remote_plugins: Mutex<HashSet<String>>,

    remote_calls: Mutex<RemoteCalls>,

    remote_instances: Mutex<HashSet<InstanceId>>,

    remote_sink: Mutex<Option<RemoteEnvelopeSink>>,

    native_handle_release_hooks: Mutex<Vec<NativeHandleReleaseHook>>,
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
                Origin::Local,
                call_id,
                instance_id,
                method.into(),
                payload,
            );
        } else {
            self.send_or_unregister(
                call_id.get(),
                Envelope::new(Frame::Call {
                    call_id,
                    plugin_id,
                    instance_id,
                    method: method.into(),
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
                Origin::Local,
                call_id,
                instance_id,
                method.into(),
                payload,
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
    /// Returns [`IstmoError::ChannelClosed`] if the outbound channel is
    /// no longer draining (usually a shutting-down runtime).
    pub fn destroy_instance(self: &Arc<Self>, instance_id: InstanceId) -> Result<(), IstmoError> {
        let plugin_id = self.routing.instance(instance_id);
        let hosted = plugin_id
            .as_deref()
            .is_some_and(|pid| lock(&self.hosts).contains_key(pid));
        if hosted {
            self.dispatch_hosted_destroy_instance(instance_id);
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
    /// - [`IstmoError::OutboundFull`] when the outbound channel is full;
    ///   the release frame is dropped.
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
            dispatch_notify(dispatcher, instance_id, method, payload);
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
    /// - [`IstmoError::OutboundFull`] when the outbound channel is full;
    ///   the cancel is dropped.
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
    pub fn inject_wire_envelope(self: &Arc<Self>, bytes: &[u8]) -> Result<(), IstmoError> {
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
                        origin,
                        call_id,
                        instance_id,
                        method,
                        payload,
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
                    dispatch_notify(dispatcher, instance_id, method, payload);
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
                self.dispatch_hosted_destroy_instance(instance_id);
                Ok(())
            }
            Frame::ReleaseNativeHandle { .. } => {
                tracing::warn!("dropped inbound frame with outbound-only variant");
                Ok(())
            }
        }
    }

    fn reject_unhosted(&self, origin: Origin, call_id: CallId, plugin_id: &str, frame: &str) {
        tracing::warn!(plugin_id = %plugin_id, frame, "inbound request for unregistered plugin");
        self.respond(
            origin,
            call_id,
            Err(encode_dispatch_error_string(&format!(
                "no host registered for `{plugin_id}`"
            ))),
            Wait::Block,
        );
    }

    /// Register a Rust-hosted plugin dispatcher.
    ///
    /// Called through [`RuntimeInit::host`] during startup; rarely used
    /// directly outside macro-generated code.
    pub fn register_host<D: Dispatch>(self: &Arc<Self>, dispatcher: D) {
        let plugin_id = dispatcher.plugin_id();
        let dispatcher: Arc<dyn Dispatch> = Arc::new(dispatcher);
        dispatcher.runtime_attached(Arc::downgrade(self));
        lock(&self.hosts).insert(plugin_id, dispatcher);
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

    /// Send a frame, blocking while the outbound channel is full.
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

    /// Put `envelope` on the outbound channel. `Ok(false)` when `wait`
    /// gave up because the call was cancelled first.
    fn push_outbound(&self, envelope: Envelope, wait: Wait<'_>) -> Result<bool, IstmoError> {
        match wait {
            Wait::Block => match self.outbound.try_send(envelope) {
                Ok(()) => Ok(true),
                Err(TrySendError::Disconnected(_)) => Err(IstmoError::ChannelClosed),
                Err(TrySendError::Full(envelope)) => {
                    match send_unless_cancelled(&self.outbound, envelope, &self.stopped) {
                        Delivery::Sent => Ok(true),
                        Delivery::Cancelled => Err(IstmoError::RuntimeShutDown),
                        Delivery::Disconnected => Err(IstmoError::ChannelClosed),
                    }
                }
            },
            Wait::UnlessCancelled(cancel) => {
                match send_unless_cancelled(&self.outbound, envelope, cancel) {
                    Delivery::Sent => Ok(true),
                    Delivery::Cancelled => Ok(false),
                    Delivery::Disconnected => Err(IstmoError::ChannelClosed),
                }
            }
            Wait::Never => {
                self.outbound
                    .try_send(envelope)
                    .map(|()| true)
                    .map_err(|err| match err {
                        TrySendError::Full(envelope) => {
                            tracing::warn!(frame = ?envelope.frame, "outbound channel full; frame dropped");
                            IstmoError::OutboundFull
                        }
                        TrySendError::Disconnected(_) => IstmoError::ChannelClosed,
                    })
            }
        }
    }

    /// Send a frame of a hosted call to whoever issued it, other than
    /// this runtime. `Ok(false)` when `wait` gave up because the call
    /// was cancelled first.
    fn send_to_issuer(
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
                    return self.push_outbound(envelope, wait);
                };
                sink(envelope.to_wire_bytes()?);
                return Ok(true);
            }
            Origin::Peer | Origin::Local => match self.route_remote(envelope)? {
                Some(envelope) => envelope,
                None => return Ok(true),
            },
        };
        self.push_outbound(envelope, wait)
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

    fn dispatch_hosted_call(
        self: &Arc<Self>,
        dispatcher: Arc<dyn Dispatch>,
        origin: Origin,
        call_id: CallId,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    ) {
        let Some(cancel) = self.begin_hosted(origin, call_id) else {
            return;
        };
        let runtime = Arc::clone(self);
        let thread_cancel = cancel.clone();
        let spawned = spawn_named("istmo-call", move || {
            // The dispatcher is shared, so a panic can leave its state
            // half-updated for later calls whether or not it is caught.
            // Catching it at least answers this call instead of leaving
            // the caller waiting forever; the generated hosts recover
            // poisoned locks as `sync::lock` does.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                pollster::block_on(dispatcher.dispatch(
                    instance_id,
                    &method,
                    &payload,
                    thread_cancel.clone(),
                ))
            }));
            runtime.finish_hosted_call(origin, call_id, &thread_cancel, outcome);
        });
        if let Err(err) = spawned {
            tracing::error!(?err, ?call_id, "failed to spawn a host thread");
            let message = DispatchError::Spawn(err).to_string();
            self.respond(
                origin,
                call_id,
                Err(encode_dispatch_error_string(&message)),
                Wait::UnlessCancelled(&cancel),
            );
            lock(&self.hosted).finish(origin, call_id);
        }
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
            self.respond(
                origin,
                call_id,
                Err(encode_dispatch_error_string(
                    &IstmoError::RuntimeShutDown.to_string(),
                )),
                Wait::Never,
            );
        }
        cancel
    }

    fn finish_hosted_call(
        self: &Arc<Self>,
        origin: Origin,
        call_id: CallId,
        cancel: &CancelToken,
        outcome: std::thread::Result<Result<Outcome, DispatchError>>,
    ) {
        let result = match outcome {
            Ok(Ok(Outcome::StreamOpened(receiver))) => {
                // The call stays active: the pump owns it until the
                // stream ends, so a cancel still reaches the producer.
                self.spawn_stream_pump(origin, call_id, cancel, receiver);
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
        // The call leaves the table only once its response is out, so a
        // cancel still frees a response waiting on a full channel.
        self.respond_unless_cancelled(origin, call_id, result, cancel);
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
        let Some(cancel) = self.begin_hosted(origin, call_id) else {
            return;
        };
        let runtime = Arc::clone(self);
        let thread_cancel = cancel.clone();
        let spawned = spawn_named("istmo-create", move || {
            // Same unwind reasoning as `dispatch_hosted_call`.
            let outcome = catch_unwind(AssertUnwindSafe(|| {
                pollster::block_on(dispatcher.create_instance(&payload, thread_cancel.clone()))
            }));
            let result = match outcome {
                Ok(Ok(Outcome::Ok(bytes))) => {
                    match crate::codec::decode::<InstanceId>(&bytes) {
                        Ok((instance_id, _)) if thread_cancel.is_cancelled() => {
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
            runtime.respond_unless_cancelled(origin, call_id, result, &thread_cancel);
            lock(&runtime.hosted).finish(origin, call_id);
        });
        if let Err(err) = spawned {
            tracing::error!(?err, ?call_id, "failed to spawn a host thread");
            let message = DispatchError::Spawn(err).to_string();
            self.respond(
                origin,
                call_id,
                Err(encode_dispatch_error_string(&message)),
                Wait::UnlessCancelled(&cancel),
            );
            lock(&self.hosted).finish(origin, call_id);
        }
    }

    fn dispatch_hosted_destroy_instance(&self, instance_id: InstanceId) {
        let Some(plugin_id) = self.routing.remove_instance(instance_id) else {
            return;
        };
        if let Some(dispatcher) = self.host(&plugin_id) {
            dispatcher.destroy_instance(instance_id);
        }
    }

    /// Forward a hosted stream's items until the producer closes or fails
    /// it, or the consumer cancels, then end it.
    fn spawn_stream_pump(
        self: &Arc<Self>,
        origin: Origin,
        call_id: CallId,
        cancel: &CancelToken,
        receiver: FlumeReceiver<HostedStreamItem>,
    ) {
        let stream_id = StreamId(call_id.get());
        let runtime = Arc::clone(self);
        let pump_cancel = cancel.clone();
        let spawned = spawn_named("istmo-stream", move || {
            let reason = runtime.pump_stream(origin, stream_id, &pump_cancel, &receiver);
            // Release the producer before the end goes out.
            drop(receiver);
            runtime.end_stream(origin, stream_id, reason, &pump_cancel);
            lock(&runtime.hosted).finish(origin, call_id);
        });
        if let Err(err) = spawned {
            tracing::error!(?err, ?stream_id, "failed to spawn a stream pump thread");
            let message = DispatchError::Spawn(err).to_string();
            self.end_stream(
                origin,
                stream_id,
                StreamEndReason::Error(encode_dispatch_error_string(&message)),
                cancel,
            );
            lock(&self.hosted).finish(origin, call_id);
        }
    }

    fn pump_stream(
        &self,
        origin: Origin,
        stream_id: StreamId,
        cancel: &CancelToken,
        receiver: &FlumeReceiver<HostedStreamItem>,
    ) -> StreamEndReason {
        enum Step {
            Item(HostedStreamItem),
            Closed,
            Cancelled,
        }
        loop {
            let step = flume::Selector::new()
                .recv(receiver, |item| item.map_or(Step::Closed, Step::Item))
                .recv(cancel.disconnect_signal(), |_| Step::Cancelled)
                .wait();
            // A producer that never stops sending keeps its receiver
            // ready, so the flag is what guarantees a cancel is seen.
            if cancel.is_cancelled() {
                return StreamEndReason::Cancelled;
            }
            match step {
                Step::Item(Ok(payload)) => {
                    if !self.emit_event(origin, stream_id, payload, cancel) {
                        return StreamEndReason::Cancelled;
                    }
                }
                Step::Item(Err(error)) => return StreamEndReason::Error(error),
                Step::Closed => return StreamEndReason::Complete,
                Step::Cancelled => return StreamEndReason::Cancelled,
            }
        }
    }

    /// Forward one hosted stream item, waiting out a full consumer or
    /// outbound channel unless the stream is cancelled meanwhile.
    /// `false` once nobody can receive further items.
    fn emit_event(
        &self,
        origin: Origin,
        stream_id: StreamId,
        payload: Vec<u8>,
        cancel: &CancelToken,
    ) -> bool {
        let delivered = match origin {
            Origin::Local => self.routing.stream_sender(stream_id).map(|tx| {
                matches!(
                    send_unless_cancelled(&tx, StreamMessage::Event(payload), cancel),
                    Delivery::Sent
                )
            }),
            Origin::Peer | Origin::Remote => self.send_to_issuer(
                origin,
                Envelope::new(Frame::Event { stream_id, payload }),
                Wait::UnlessCancelled(cancel),
            ),
        };
        delivered
            .inspect_err(|err| tracing::debug!(?err, ?stream_id, "hosted stream event undelivered"))
            .unwrap_or(false)
    }

    /// Send a hosted stream's end. Once the stream is cancelled the end
    /// is best effort and never blocks: its issuer left, or the runtime
    /// shut down and the transport may have stopped draining.
    fn end_stream(
        &self,
        origin: Origin,
        stream_id: StreamId,
        reason: StreamEndReason,
        cancel: &CancelToken,
    ) {
        let delivered = match origin {
            Origin::Local => self
                .routing
                .deliver_stream_end(stream_id, reason)
                .map(|()| true),
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
            }
        };
        if let Err(err) = delivered {
            tracing::debug!(?err, ?stream_id, "hosted stream end undelivered");
        }
    }

    /// Deliver a finished hosted call's result, unless the issuer
    /// cancelled it: a cancelled call from another runtime is answered
    /// by nobody.
    fn respond_unless_cancelled(
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
        self.respond(origin, call_id, result, Wait::UnlessCancelled(cancel));
    }

    /// Deliver a hosted call's result to whoever issued it.
    fn respond(&self, origin: Origin, call_id: CallId, result: CallResult, wait: Wait<'_>) {
        if origin == Origin::Local {
            if let Err(err) = self.routing.complete(call_id, result) {
                tracing::debug!(?err, ?call_id, "hosted call finished after its caller left");
            }
            return;
        }
        let envelope = Envelope::new(Frame::Respond { call_id, result });
        match self.send_to_issuer(origin, envelope, wait) {
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
    /// streams have their [`CancelToken`]s tripped, and the remote and
    /// instance tables are cleared. The outbound channel itself stays
    /// open until the runtime is dropped, so frames already queued still
    /// reach a transport that keeps draining.
    ///
    /// Host threads still running finish on their own: their responses
    /// to other runtimes are discarded, and their streams stop at the
    /// next item, so none of them waits on a transport that stopped
    /// draining.
    pub fn shutdown(&self) {
        self.shutting_down.store(true, Ordering::Release);
        self.stopped.cancel();
        let count = self.routing.clear();
        lock(&self.hosted).cancel_all();
        *lock(&self.remote_calls) = RemoteCalls::default();
        lock(&self.remote_instances).clear();
        if count > 0 {
            tracing::info!(count, "istmo runtime shutdown cancelled pending operations");
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
    /// Not at all: a full channel drops the frame.
    Never,
}

/// What became of a [`send_unless_cancelled`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    Sent,
    Cancelled,
    Disconnected,
}

/// Send `message`, waiting while `tx` is full, unless `cancel` trips
/// first.
fn send_unless_cancelled<T>(tx: &FlumeSender<T>, message: T, cancel: &CancelToken) -> Delivery {
    if cancel.is_cancelled() {
        return Delivery::Cancelled;
    }
    let message = match tx.try_send(message) {
        Ok(()) => return Delivery::Sent,
        Err(TrySendError::Disconnected(_)) => return Delivery::Disconnected,
        Err(TrySendError::Full(message)) => message,
    };
    flume::Selector::new()
        .send(tx, message, |sent| {
            if sent.is_ok() {
                Delivery::Sent
            } else {
                Delivery::Disconnected
            }
        })
        .recv(cancel.disconnect_signal(), |_| Delivery::Cancelled)
        .wait()
}

/// Run a notification on its own thread; nothing is sent back.
fn dispatch_notify(
    dispatcher: Arc<dyn Dispatch>,
    instance_id: Option<InstanceId>,
    method: String,
    payload: Vec<u8>,
) {
    let spawned = spawn_named("istmo-notify", move || {
        // Same unwind reasoning as `Runtime::dispatch_hosted_call`.
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            pollster::block_on(dispatcher.dispatch(
                instance_id,
                &method,
                &payload,
                CancelToken::new(),
            ))
        }));
        match outcome {
            Ok(Ok(_)) => {}
            Ok(Err(err)) => tracing::warn!(?err, method = %method, "notify dispatch failed"),
            Err(_) => tracing::error!(method = %method, "notify dispatch panicked"),
        }
    });
    if let Err(err) = spawned {
        tracing::error!(
            ?err,
            "failed to spawn a notify thread; notification dropped"
        );
    }
}

/// Spawn a named thread, surfacing the OS refusing one as an error
/// instead of the panic [`std::thread::spawn`] raises.
fn spawn_named(name: &str, body: impl FnOnce() + Send + 'static) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .map(drop)
}

fn encode_dispatch_error_string(message: &str) -> Vec<u8> {
    crate::codec::encode(&message.to_owned()).unwrap_or_default()
}

fn build(config: RuntimeConfig) -> RuntimeInit {
    let (outbound_tx, outbound_rx) = bounded(config.outbound_capacity);
    let runtime = Arc::new(Runtime {
        outbound: outbound_tx,
        routing: RoutingTables::default(),
        early_events: Arc::new(EarlyEventStore::new()),
        main_thread: config.main_thread,
        next_id: AtomicU64::new(1),
        next_native_handle: AtomicU64::new(NativeHandleId::RUST_ALLOCATED_BASE),
        shutting_down: AtomicBool::new(false),
        stopped: CancelToken::new(),
        declared_plugins: Mutex::new(HashSet::new()),
        enforce_declarations: AtomicBool::new(false),
        hosts: Mutex::new(HashMap::new()),
        hosted: Mutex::new(HostedCalls::default()),
        remote_plugins: Mutex::new(HashSet::new()),
        remote_calls: Mutex::new(RemoteCalls::default()),
        remote_instances: Mutex::new(HashSet::new()),
        remote_sink: Mutex::new(None),
        native_handle_release_hooks: Mutex::new(Vec::new()),
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
        let rx = self.receiver.take().ok_or(IstmoError::ChannelClosed)?;
        rx.recv().map_err(|_| IstmoError::ChannelClosed)
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
        self.receiver
            .recv()
            .map(|message| self.observe(message))
            .map_err(|_| IstmoError::ChannelClosed)
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

#[cfg(test)]
mod tests {
    use super::*;

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
