use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, bounded};

use crate::dispatch::{CancelToken, Dispatch, Outcome, Plugin};
use crate::early_events::EarlyEventStore;
use crate::error::IstmoError;
use crate::main_thread::{InlineMainThread, MainThread};
use crate::protocol::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId, PROTOCOL_VERSION, StreamId,
};
use crate::routing::{CallResult, InstanceEntry, RoutingTables, StreamMessage};
use crate::sync::lock;

/// Default depth of the outbound envelope channel.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 256;

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
/// [`istmo-android`]: https://docs.rs/istmo-android
/// [`istmo-ios`]: https://docs.rs/istmo-ios
pub struct Runtime {
    outbound: FlumeSender<Envelope>,
    routing: Arc<RoutingTables>,
    early_events: Arc<EarlyEventStore>,
    main_thread: Arc<dyn MainThread>,
    next_id: AtomicU64,

    declared_plugins: Mutex<HashSet<&'static str>>,

    enforce_declarations: std::sync::atomic::AtomicBool,

    hosts: Mutex<HashMap<&'static str, Arc<dyn Dispatch>>>,

    cancelled_hosted: Mutex<HashMap<CallId, CancelToken>>,

    remote_plugins: Mutex<HashSet<String>>,

    remote_calls: Mutex<HashSet<CallId>>,

    remote_streams: Mutex<HashSet<StreamId>>,

    remote_instances: Mutex<HashSet<InstanceId>>,

    remote_sink: Mutex<Option<RemoteEnvelopeSink>>,

    native_handle_release_hooks: Mutex<Vec<NativeHandleReleaseHook>>,
}

impl std::fmt::Debug for Runtime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Runtime")
            .field("next_id", &self.next_id.load(Ordering::Relaxed))
            .field("main_thread", &self.main_thread)
            .finish_non_exhaustive()
    }
}

static GLOBAL: OnceLock<Arc<Runtime>> = OnceLock::new();

impl Runtime {
    /// Build a runtime and install it as the process-global singleton.
    ///
    /// Called once at app startup by the platform transport, typically
    /// through the [`istmo::runtime!`](../../istmo_macros/macro.runtime.html)
    /// macro.
    ///
    /// # Errors
    ///
    /// Returns [`IstmoError::RuntimeAlreadyStarted`] if another
    /// [`Runtime`] has already been installed in this process.
    pub fn init(config: RuntimeConfig) -> Result<RuntimeInit, IstmoError> {
        let init = build(config);
        GLOBAL
            .set(init.runtime.clone())
            .map_err(|_| IstmoError::RuntimeAlreadyStarted)?;
        Ok(init)
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

    /// Access the runtime's routing tables.
    #[must_use]
    pub const fn routing(&self) -> &Arc<RoutingTables> {
        &self.routing
    }

    #[must_use]
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Invoke a plugin method and return a [`CallHandle`] that resolves
    /// to the response.
    ///
    /// The handle implements [`Future`] and can be `.await`ed directly;
    /// it can also be resolved synchronously with
    /// [`CallHandle::recv_blocking`]. Dropping the handle before the
    /// response arrives cancels the call.
    ///
    /// Method arguments are passed as pre-encoded bincode `payload`
    /// bytes — usually produced by the client generated by
    /// [`#[plugin]`](../../istmo_macros/attr.plugin.html), so callers
    /// rarely construct these frames by hand.
    ///
    /// # Errors
    ///
    /// Any of the routing / channel-closed variants of [`IstmoError`].
    pub fn call(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        let plugin_id = plugin_id.into();
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
        let hosted = lock(&self.hosts).contains_key(plugin_id.as_str());
        let envelope = Envelope::new(Frame::Call {
            call_id,
            plugin_id,
            instance_id,
            method: method.into(),
            payload,
        });
        if hosted {
            self.dispatch_inbound(envelope)?;
        } else {
            self.send_outbound(envelope)?;
        }
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
        })
    }

    /// Open a stream-returning plugin method.
    ///
    /// Returns a [`StreamHandle`] whose [`recv`](StreamHandle::recv)
    /// blocks on the next item until the peer signals end-of-stream.
    /// `capacity` controls how many items may buffer before the producer
    /// blocks.
    ///
    /// # Errors
    ///
    /// Any of the routing / channel-closed variants of [`IstmoError`].
    pub fn stream(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
        capacity: usize,
    ) -> Result<StreamHandle, IstmoError> {
        let plugin_id = plugin_id.into();
        let id = self.next_id();
        let call_id = CallId(id);
        let stream_id = StreamId(id);
        let rx = self.routing.register_stream(stream_id, capacity);
        let hosted = lock(&self.hosts).contains_key(plugin_id.as_str());
        let envelope = Envelope::new(Frame::Call {
            call_id,
            plugin_id,
            instance_id,
            method: method.into(),
            payload,
        });
        if hosted {
            self.dispatch_inbound(envelope)?;
        } else {
            self.send_outbound(envelope)?;
        }
        Ok(StreamHandle {
            stream_id,
            receiver: rx,
            runtime: Arc::downgrade(self),
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
    /// Any of the routing / channel-closed variants of [`IstmoError`].
    pub fn create_instance(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        let plugin_id = plugin_id.into();
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
        let hosted = lock(&self.hosts).contains_key(plugin_id.as_str());
        let envelope = Envelope::new(Frame::CreateInstance {
            call_id,
            plugin_id,
            payload,
        });
        if hosted {
            self.dispatch_inbound(envelope)?;
        } else {
            self.send_outbound(envelope)?;
        }
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
        })
    }

    /// Record a locally-hosted plugin instance in the routing tables so
    /// subsequent inbound calls can find it.
    pub fn register_instance(&self, instance_id: InstanceId, plugin_id: impl Into<String>) {
        self.routing.register_instance(
            instance_id,
            InstanceEntry {
                plugin_id: plugin_id.into(),
            },
        );
    }

    /// Release a previously created plugin instance and notify the peer.
    ///
    /// # Errors
    ///
    /// Returns [`IstmoError::ChannelClosed`] if the outbound channel is
    /// no longer draining (usually a shutting-down runtime).
    pub fn destroy_instance(self: &Arc<Self>, instance_id: InstanceId) -> Result<(), IstmoError> {
        let plugin_id = self.routing.instance(instance_id).map(|e| e.plugin_id);
        let hosted = plugin_id
            .as_deref()
            .is_some_and(|pid| lock(&self.hosts).contains_key(pid));
        let envelope = Envelope::new(Frame::DestroyInstance { instance_id });
        if hosted {
            self.dispatch_inbound(envelope)?;
        } else {
            self.routing.remove_instance(instance_id);
            self.send_outbound(envelope)?;
        }
        Ok(())
    }

    /// Notify the peer that it can drop the platform object backing
    /// `handle_id`.
    ///
    /// Called from the [`Drop`] impl of [`NativeHandle`](crate::NativeHandle).
    ///
    /// Every hook registered via
    /// [`install_native_handle_release_hook`](Self::install_native_handle_release_hook)
    /// fires before the wire frame is enqueued — hooks are the release
    /// route for Rust-side hosts whose handles are consumed by clients
    /// living inside the same process (no external transport pump).
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] when the outbound pump is gone.
    pub fn release_native_handle(&self, handle_id: NativeHandleId) -> Result<(), IstmoError> {
        let hooks: Vec<NativeHandleReleaseHook> =
            lock(&self.native_handle_release_hooks).clone();
        for hook in &hooks {
            hook(handle_id);
        }
        let envelope = Envelope::new(Frame::ReleaseNativeHandle { handle_id });
        self.send_outbound(envelope)
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
    /// [`IstmoError::ChannelClosed`] when the outbound pump is gone.
    pub fn notify(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<(), IstmoError> {
        let plugin_id = plugin_id.into();
        let method = method.into();
        if lock(&self.hosts).contains_key(plugin_id.as_str()) {
            self.dispatch_notify(&plugin_id, instance_id, method, payload);
            return Ok(());
        }
        let envelope = Envelope::new(Frame::Notify {
            plugin_id,
            instance_id,
            method,
            payload,
        });
        self.send_outbound(envelope)
    }

    fn dispatch_notify(
        self: &Arc<Self>,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    ) {
        let dispatcher = lock(&self.hosts).get(plugin_id).cloned();
        let Some(dispatcher) = dispatcher else {
            tracing::warn!(plugin_id = %plugin_id, "inbound Notify for unregistered plugin");
            return;
        };
        std::thread::spawn(move || {
            let cancel = crate::dispatch::CancelToken::new();
            let outcome = pollster::block_on(async {
                dispatcher
                    .dispatch(instance_id, &method, &payload, cancel)
                    .await
            });
            if let Err(err) = outcome {
                tracing::warn!(?err, method = %method, "notify dispatch failed");
            }
        });
    }

    /// Signal cancellation of an in-flight call.
    ///
    /// Locally hosted calls fire their [`CancelToken`]; remote calls
    /// emit a [`Frame::Cancel`] to the peer.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] if the outbound pump is gone.
    pub fn cancel_call(&self, call_id: CallId) -> Result<(), IstmoError> {
        self.routing.remove_pending(call_id.get());
        let envelope = Envelope::new(Frame::Cancel { call_id });
        self.send_outbound(envelope)
    }

    /// Ask the peer to close a live stream.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] if the outbound pump is gone.
    pub fn cancel_stream(&self, stream_id: StreamId) -> Result<(), IstmoError> {
        self.routing.remove_pending(stream_id.get());
        let envelope = Envelope::new(Frame::Cancel {
            call_id: CallId(stream_id.get()),
        });
        self.send_outbound(envelope)
    }

    /// Decode a wire envelope and dispatch it through the runtime.
    ///
    /// Used by transport crates to hand received bytes back to the
    /// runtime.
    ///
    /// # Errors
    ///
    /// Any variant of [`IstmoError`] surfaced by
    /// [`Envelope::from_wire_bytes`] or [`Runtime::dispatch_inbound`].
    pub fn inject_wire_envelope(self: &Arc<Self>, bytes: &[u8]) -> Result<(), IstmoError> {
        let envelope = Envelope::from_wire_bytes(bytes)?;
        self.dispatch_inbound(envelope)
    }

    /// Dispatch an already-decoded envelope.
    ///
    /// Routes call frames to the matching registered dispatcher,
    /// completes pending [`CallHandle`]s from response frames, forwards
    /// stream events, and so on.
    ///
    /// # Errors
    ///
    /// [`IstmoError::UnknownPlugin`], [`IstmoError::UnknownInstance`],
    /// [`IstmoError::UnknownCallId`] or [`IstmoError::UnknownStreamId`]
    /// when the frame references something the routing tables don't know
    /// about.
    pub fn dispatch_inbound(self: &Arc<Self>, envelope: Envelope) -> Result<(), IstmoError> {
        if envelope.version != PROTOCOL_VERSION {
            return Err(IstmoError::ProtocolVersionMismatch {
                expected: PROTOCOL_VERSION,
                got: envelope.version,
            });
        }
        match envelope.frame {
            Frame::Respond { call_id, result } => self.routing.deliver_response(call_id, result),
            Frame::Event { stream_id, payload } => self.routing.deliver_event(stream_id, payload),
            Frame::StreamEnd { stream_id, reason } => {
                self.routing.deliver_stream_end(stream_id, reason)
            }
            Frame::Call {
                call_id,
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                self.dispatch_hosted_call(call_id, &plugin_id, instance_id, method, payload);
                Ok(())
            }
            Frame::Cancel { call_id } => {
                self.cancel_hosted(call_id);
                Ok(())
            }
            Frame::EarlyEvent {
                channel,
                kind,
                payload,
            } => {
                match kind {
                    EarlyEventKind::Latest => self.publish_early_latest(&channel, payload),
                    EarlyEventKind::Queue { capacity } => {
                        self.publish_early_queue(&channel, capacity as usize, payload);
                    }
                }
                Ok(())
            }
            Frame::Notify {
                plugin_id,
                instance_id,
                method,
                payload,
            } => {
                self.dispatch_notify(&plugin_id, instance_id, method, payload);
                Ok(())
            }
            Frame::CreateInstance {
                call_id,
                plugin_id,
                payload,
            } => {
                self.dispatch_hosted_create_instance(call_id, plugin_id, payload);
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

    #[must_use]
    /// Return `true` if `plugin_id` has been declared on this runtime.
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

    #[must_use]
    /// Return `true` if `plugin_id` is routed through the remote sink.
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

    fn send_outbound(&self, envelope: Envelope) -> Result<(), IstmoError> {
        let sink = lock(&self.remote_sink).clone();
        if sink.is_some() && self.classify_and_track(&envelope.frame) {
            let bytes = envelope.to_wire_bytes()?;

            if let Some(sink) = sink {
                sink(bytes);
                return Ok(());
            }
        }
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)
    }

    fn classify_and_track(&self, frame: &Frame) -> bool {
        match frame {
            Frame::Call {
                call_id, plugin_id, ..
            } => {
                if lock(&self.remote_plugins).contains(plugin_id) {
                    lock(&self.remote_calls).insert(*call_id);
                    lock(&self.remote_streams).insert(StreamId(call_id.get()));
                    true
                } else {
                    false
                }
            }
            Frame::CreateInstance {
                call_id, plugin_id, ..
            } => {
                if lock(&self.remote_plugins).contains(plugin_id) {
                    lock(&self.remote_calls).insert(*call_id);
                    true
                } else {
                    false
                }
            }
            Frame::Notify { plugin_id, .. } => lock(&self.remote_plugins).contains(plugin_id),
            Frame::Respond { call_id, .. } => {
                let mut remote_calls = lock(&self.remote_calls);
                remote_calls.remove(call_id)
            }
            Frame::Cancel { call_id } => {
                let hit = lock(&self.remote_calls).contains(call_id);
                if hit {
                    lock(&self.remote_calls).remove(call_id);
                    lock(&self.remote_streams).remove(&StreamId(call_id.get()));
                }
                hit
            }
            Frame::Event { stream_id, .. } => lock(&self.remote_streams).contains(stream_id),
            Frame::StreamEnd { stream_id, .. } => {
                let mut remote_streams = lock(&self.remote_streams);
                remote_streams.remove(stream_id)
            }
            Frame::DestroyInstance { instance_id } => {
                let mut remote_instances = lock(&self.remote_instances);
                remote_instances.remove(instance_id)
            }
            Frame::EarlyEvent { .. } | Frame::ReleaseNativeHandle { .. } => false,
        }
    }

    /// Record that a given [`InstanceId`] lives in a remote process.
    ///
    /// Called by the remote-bridge receive side after adopting a
    /// [`Frame::CreateInstance`] response so the runtime routes future
    /// calls to the correct sink.
    pub fn mark_instance_remote(&self, instance_id: InstanceId) {
        lock(&self.remote_instances).insert(instance_id);
    }

    /// Record that a given [`CallId`] originated on a remote process.
    ///
    /// Used by the remote-bridge receive side so the eventual
    /// [`Frame::Respond`] is steered back through the sink instead of
    /// completing a local call handle.
    pub fn mark_call_remote(&self, call_id: CallId) {
        lock(&self.remote_calls).insert(call_id);
        lock(&self.remote_streams).insert(StreamId(call_id.get()));
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

    fn cancel_hosted(&self, call_id: CallId) {
        let token = lock(&self.cancelled_hosted)
            .entry(call_id)
            .or_default()
            .clone();
        token.cancel();
    }

    fn dispatch_hosted_call(
        self: &Arc<Self>,
        call_id: CallId,
        plugin_id: &str,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    ) {
        let dispatcher = lock(&self.hosts).get(plugin_id).cloned();
        let Some(dispatcher) = dispatcher else {
            tracing::warn!(plugin_id = %plugin_id, "inbound Call for unregistered plugin");
            self.send_respond(
                call_id,
                Err(encode_dispatch_error_string(&format!(
                    "no host registered for `{plugin_id}`"
                ))),
            );
            return;
        };
        let cancel = lock(&self.cancelled_hosted)
            .entry(call_id)
            .or_default()
            .clone();
        let runtime = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome = pollster::block_on(async {
                dispatcher
                    .dispatch(instance_id, &method, &payload, cancel)
                    .await
            });
            let cancelled = {
                let mut map = lock(&runtime.cancelled_hosted);
                map.remove(&call_id).is_some_and(|t| t.is_cancelled())
            };
            if cancelled {
                tracing::debug!(?call_id, "hosted call cancelled; discarding response");
                return;
            }
            match outcome {
                Ok(Outcome::Ok(bytes)) => runtime.send_respond(call_id, Ok(bytes)),
                Ok(Outcome::DomainError(bytes)) => runtime.send_respond(call_id, Err(bytes)),
                Ok(Outcome::StreamOpened(receiver)) => {
                    runtime.spawn_stream_pump(StreamId(call_id.get()), receiver);
                }
                Err(err) => {
                    tracing::error!(?err, ?call_id, "hosted dispatch failed");
                    runtime
                        .send_respond(call_id, Err(encode_dispatch_error_string(&err.to_string())));
                }
            }
        });
    }

    fn dispatch_hosted_create_instance(
        self: &Arc<Self>,
        call_id: CallId,
        plugin_id: String,
        payload: Vec<u8>,
    ) {
        let dispatcher = lock(&self.hosts).get(plugin_id.as_str()).cloned();
        let Some(dispatcher) = dispatcher else {
            tracing::warn!(plugin_id = %plugin_id, "inbound CreateInstance for unregistered plugin");
            self.send_respond(
                call_id,
                Err(encode_dispatch_error_string(&format!(
                    "no host registered for `{plugin_id}`"
                ))),
            );
            return;
        };
        let cancel = lock(&self.cancelled_hosted)
            .entry(call_id)
            .or_default()
            .clone();
        let runtime = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome =
                pollster::block_on(async { dispatcher.create_instance(&payload, cancel).await });
            let cancelled = {
                let mut map = lock(&runtime.cancelled_hosted);
                map.remove(&call_id).is_some_and(|t| t.is_cancelled())
            };
            if cancelled {
                tracing::debug!(
                    ?call_id,
                    "hosted create_instance cancelled; discarding response"
                );
                return;
            }
            match outcome {
                Ok(Outcome::Ok(bytes)) => {
                    if let Ok((instance_id, _)) = crate::codec::decode::<InstanceId>(&bytes) {
                        runtime.register_instance(instance_id, plugin_id.clone());
                    } else {
                        tracing::error!(
                            ?call_id,
                            "hosted create_instance returned undecodable InstanceId bytes",
                        );
                    }
                    runtime.send_respond(call_id, Ok(bytes));
                }
                Ok(Outcome::DomainError(bytes)) => runtime.send_respond(call_id, Err(bytes)),
                Ok(Outcome::StreamOpened(_)) => {
                    tracing::error!(
                        ?call_id,
                        "hosted create_instance returned StreamOpened; refusing",
                    );
                    runtime.send_respond(
                        call_id,
                        Err(encode_dispatch_error_string(
                            "create_instance returned StreamOpened",
                        )),
                    );
                }
                Err(err) => {
                    tracing::error!(?err, ?call_id, "hosted create_instance failed");
                    runtime
                        .send_respond(call_id, Err(encode_dispatch_error_string(&err.to_string())));
                }
            }
        });
    }

    fn dispatch_hosted_destroy_instance(self: &Arc<Self>, instance_id: InstanceId) {
        let plugin_id = self
            .routing
            .instance(instance_id)
            .map(|entry| entry.plugin_id);
        self.routing.remove_instance(instance_id);
        let Some(plugin_id) = plugin_id else {
            return;
        };
        let dispatcher = lock(&self.hosts).get(plugin_id.as_str()).cloned();
        if let Some(dispatcher) = dispatcher {
            dispatcher.destroy_instance(instance_id);
        }
    }

    fn spawn_stream_pump(
        self: &Arc<Self>,
        stream_id: StreamId,
        receiver: flume::Receiver<Vec<u8>>,
    ) {
        let runtime = Arc::clone(self);
        std::thread::spawn(move || {
            while let Ok(bytes) = receiver.recv() {
                runtime.send_event(stream_id, bytes);
            }
            runtime.send_stream_end(stream_id, crate::protocol::StreamEndReason::Complete);
        });
    }

    fn send_event(self: &Arc<Self>, stream_id: StreamId, payload: Vec<u8>) {
        if self.routing.has_pending(stream_id.get()) {
            if let Err(err) = self.routing.deliver_event(stream_id, payload) {
                tracing::warn!(?err, "local Event delivery failed");
            }
            return;
        }
        let envelope = Envelope::new(Frame::Event { stream_id, payload });
        if let Err(err) = self.send_outbound(envelope) {
            tracing::warn!(?err, "failed to send Event frame; outbound channel closed");
        }
    }

    fn send_stream_end(
        self: &Arc<Self>,
        stream_id: StreamId,
        reason: crate::protocol::StreamEndReason,
    ) {
        if self.routing.has_pending(stream_id.get()) {
            if let Err(err) = self.routing.deliver_stream_end(stream_id, reason) {
                tracing::warn!(?err, "local StreamEnd delivery failed");
            }
            return;
        }
        let envelope = Envelope::new(Frame::StreamEnd { stream_id, reason });
        if let Err(err) = self.send_outbound(envelope) {
            tracing::warn!(
                ?err,
                "failed to send StreamEnd frame; outbound channel closed"
            );
        }
    }

    fn send_respond(&self, call_id: CallId, result: CallResult) {
        if self.routing.has_pending(call_id.get()) {
            if let Err(err) = self.routing.deliver_response(call_id, result) {
                tracing::warn!(?err, "local Respond delivery failed");
            }
            return;
        }
        let envelope = Envelope::new(Frame::Respond { call_id, result });
        if let Err(err) = self.send_outbound(envelope) {
            tracing::warn!(
                ?err,
                "failed to send Respond frame; outbound channel closed"
            );
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
    /// Retains up to `capacity` values; overflow drops the oldest entry.
    /// Late subscribers receive the buffered items in publish order.
    pub fn publish_early_queue(&self, channel: &str, capacity: usize, payload: Vec<u8>) {
        self.early_events.queue(channel, capacity).publish(payload);
    }

    /// Drop every pending call, close the outbound channel, and mark
    /// the runtime as no longer accepting work.
    pub fn shutdown(&self) {
        let count = self.routing.cancel_all_pending();
        if count > 0 {
            tracing::info!(count, "istmo runtime shutdown cancelled pending operations");
        }
    }
}

fn encode_dispatch_error_string(message: &str) -> Vec<u8> {
    crate::codec::encode(&message.to_owned()).unwrap_or_default()
}

fn build(config: RuntimeConfig) -> RuntimeInit {
    let (outbound_tx, outbound_rx) = bounded(config.outbound_capacity);
    let runtime = Arc::new(Runtime {
        outbound: outbound_tx,
        routing: Arc::new(RoutingTables::new()),
        early_events: Arc::new(EarlyEventStore::new()),
        main_thread: config.main_thread,
        next_id: AtomicU64::new(1),
        declared_plugins: Mutex::new(HashSet::new()),
        enforce_declarations: std::sync::atomic::AtomicBool::new(false),
        hosts: Mutex::new(HashMap::new()),
        cancelled_hosted: Mutex::new(HashMap::new()),
        remote_plugins: Mutex::new(HashSet::new()),
        remote_calls: Mutex::new(HashSet::new()),
        remote_streams: Mutex::new(HashSet::new()),
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
/// before the response arrives sends a [`Frame::Cancel`] to the peer.
#[derive(Debug)]
pub struct CallHandle {
    call_id: CallId,
    receiver: Option<oneshot::Receiver<CallResult>>,
    runtime: Weak<Runtime>,
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
        if let Some(rt) = self.runtime.upgrade() {
            drop(rt.cancel_call(self.call_id));
        }
    }
}

/// Handle to a live event stream.
///
/// Returned by [`Runtime::stream`]. Items are delivered in publish
/// order; dropping the handle asks the peer to close the stream. All
/// three receive shapes (blocking, async, non-blocking peek) are
/// available.
#[derive(Debug)]
pub struct StreamHandle {
    stream_id: StreamId,
    receiver: FlumeReceiver<StreamMessage>,
    runtime: Weak<Runtime>,
}

impl StreamHandle {
    /// The [`StreamId`] backing this handle.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    /// Block the current thread until the next stream message arrives.
    ///
    /// # Errors
    ///
    /// [`IstmoError::ChannelClosed`] if the stream's channel closes
    /// without delivering another value.
    pub fn recv(&self) -> Result<StreamMessage, IstmoError> {
        self.receiver.recv().map_err(|_| IstmoError::ChannelClosed)
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
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Non-blocking peek. Returns `None` if no message is buffered.
    #[must_use]
    pub fn try_recv(&self) -> Option<StreamMessage> {
        self.receiver.try_recv().ok()
    }
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        if let Some(rt) = self.runtime.upgrade() {
            drop(rt.cancel_stream(self.stream_id));
        }
    }
}
