//! Per-process runtime: the plumbing hub every plugin talks to.
//!
//! The runtime holds no domain state. It owns:
//!
//! * The outbound channel to the platform backend.
//! * The routing tables mapping in-flight `call_id`s / `stream_id`s back to
//!   their Rust futures / streams.
//! * The main-thread dispatcher abstraction.
//! * The early-event store.
//! * A single monotonic id counter shared by call, stream and instance ids —
//!   they occupy different tables and are wrapped in distinct newtypes, so
//!   sharing the counter is safe and reduces bookkeeping.
//!
//! [`Runtime::init`] installs a process-global instance behind an
//! [`OnceLock`]; [`Runtime::global`] returns it. Tests and multi-runtime
//! scenarios can bypass the global entirely via [`Runtime::mock`] plus
//! explicit `Arc<Runtime>` passing.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::task::{Context, Poll};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, bounded};

use crate::dispatch::{Dispatch, Outcome, Plugin};
use crate::early_events::EarlyEventStore;
use crate::error::IstmoError;
use crate::main_thread::{InlineMainThread, MainThread};
use crate::protocol::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId, PROTOCOL_VERSION, StreamId,
};
use crate::routing::{CallResult, InstanceEntry, RoutingTables, StreamMessage};
use crate::sync::lock;

/// Default bounded capacity of the outbound frame channel.
pub const DEFAULT_OUTBOUND_CAPACITY: usize = 256;

/// Configuration for a real (non-mock) runtime.
#[derive(Debug)]
pub struct RuntimeConfig {
    /// Dispatcher used to schedule work back onto the platform main thread.
    pub main_thread: Arc<dyn MainThread>,
    /// Bounded capacity of the outbound frame channel.
    pub outbound_capacity: usize,
}

impl RuntimeConfig {
    /// Config with an [`InlineMainThread`] dispatcher and the default outbound
    /// capacity. Suitable for desktop mock backends.
    #[must_use]
    pub fn inline() -> Self {
        Self {
            main_thread: Arc::new(InlineMainThread),
            outbound_capacity: DEFAULT_OUTBOUND_CAPACITY,
        }
    }
}

/// Bundle returned by [`Runtime::init`] or [`Runtime::mock`]: the runtime
/// itself and the receiver end of the outbound channel that a platform
/// backend must drain.
///
/// Supports a small builder API — [`Self::host`], [`Self::expects`] and
/// [`Self::finish`] — so the `istmo::runtime!` macro and tests can write:
/// `Runtime::mock().host(Hosted::new(MockX)).expects::<Perms>().finish()`.
#[derive(Debug)]
pub struct RuntimeInit {
    pub runtime: Arc<Runtime>,
    pub outbound: FlumeReceiver<Envelope>,
}

impl RuntimeInit {
    /// Registers a hosted plugin dispatcher on the runtime and returns
    /// `self` so the call chains.
    #[must_use]
    pub fn host<D>(self, dispatcher: D) -> Self
    where
        D: Dispatch,
    {
        self.runtime.register_host(dispatcher);
        self
    }

    /// Declares that this process's client side will `acquire()` `T`.
    /// Chains for use inside the `istmo::runtime!` `plugins:` list.
    #[must_use]
    pub fn expects<T>(self) -> Self
    where
        T: Plugin,
    {
        self.runtime.declare_plugin(T::PLUGIN_ID);
        self
    }

    /// Turns on strict declaration enforcement. Call once after every
    /// [`Self::expects`] entry has been registered — subsequent
    /// `acquire()` calls for undeclared plugins fail fast with
    /// [`IstmoError::PluginNotDeclared`].
    #[must_use]
    pub fn finish(self) -> Self {
        self.runtime.set_enforce_declarations(true);
        self
    }
}

/// Process-scoped runtime. Never construct directly — use [`Runtime::init`]
/// or [`Runtime::mock`].
pub struct Runtime {
    outbound: FlumeSender<Envelope>,
    routing: Arc<RoutingTables>,
    early_events: Arc<EarlyEventStore>,
    main_thread: Arc<dyn MainThread>,
    next_id: AtomicU64,
    /// Declared plugin ids: those the process's client side is allowed to
    /// `acquire()`. Populated by [`Self::declare_plugin`] via the
    /// `istmo::runtime!` macro's `plugins:` list.
    declared_plugins: Mutex<HashSet<&'static str>>,
    /// When true, [`Self::check_declared`] rejects ids missing from
    /// [`Self::declared_plugins`]. Set by `istmo::runtime!` after all
    /// plugins are declared. Mocks default to permissive so tests can
    /// construct clients without a full declaration list.
    enforce_declarations: std::sync::atomic::AtomicBool,
    /// Server-side dispatchers keyed by plugin id — populated by
    /// [`Self::register_host`].
    hosts: Mutex<HashMap<&'static str, Arc<dyn Dispatch>>>,
    /// Set of hosted call ids that have been cancelled by an inbound Cancel
    /// frame. The dispatcher thread checks this before submitting its
    /// Respond frame so the response is dropped rather than raced with a
    /// stale reply.
    cancelled_hosted: Mutex<HashSet<CallId>>,
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
    /// Installs the process-global runtime. Fails with
    /// [`IstmoError::RuntimeAlreadyStarted`] if a previous call already
    /// succeeded.
    pub fn init(config: RuntimeConfig) -> Result<RuntimeInit, IstmoError> {
        let init = build(config);
        GLOBAL
            .set(init.runtime.clone())
            .map_err(|_| IstmoError::RuntimeAlreadyStarted)?;
        Ok(init)
    }

    /// Returns the process-global runtime, or [`IstmoError::RuntimeNotStarted`]
    /// if [`Runtime::init`] has not been called yet.
    pub fn global() -> Result<Arc<Self>, IstmoError> {
        GLOBAL.get().cloned().ok_or(IstmoError::RuntimeNotStarted)
    }

    /// Builds a runtime backed by an inline main-thread dispatcher, without
    /// touching the process global. Intended for tests and multi-runtime
    /// scenarios.
    #[must_use]
    pub fn mock() -> RuntimeInit {
        build(RuntimeConfig::inline())
    }

    /// Access to the main-thread dispatcher configured for this runtime.
    #[must_use]
    pub const fn main_thread(&self) -> &Arc<dyn MainThread> {
        &self.main_thread
    }

    /// Access to the early-event store.
    #[must_use]
    pub const fn early_events(&self) -> &Arc<EarlyEventStore> {
        &self.early_events
    }

    /// Access to the routing tables. Reserved for the platform backend and
    /// generated plugin glue.
    #[must_use]
    pub const fn routing(&self) -> &Arc<RoutingTables> {
        &self.routing
    }

    /// Allocates a fresh id from the monotonic counter.
    #[must_use]
    fn next_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Fires a unary `Call` and returns a [`CallHandle`] that resolves to the
    /// response.
    ///
    /// If the plugin is hosted on this same runtime (`hosts:` side of
    /// `istmo::runtime!`), the Call is dispatched locally and never touches
    /// the outbound channel — a same-runtime loop is a zero-wire-hop
    /// round-trip. Otherwise the frame goes out via the platform pump.
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
            self.outbound
                .send(envelope)
                .map_err(|_| IstmoError::ChannelClosed)?;
        }
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
        })
    }

    /// Opens a stream. `capacity == 0` requests an unbounded channel;
    /// positive values apply back-pressure at the receiver.
    pub fn stream(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
        capacity: usize,
    ) -> Result<StreamHandle, IstmoError> {
        let id = self.next_id();
        let call_id = CallId(id);
        let stream_id = StreamId(id);
        let rx = self.routing.register_stream(stream_id, capacity);
        let envelope = Envelope::new(Frame::Call {
            call_id,
            plugin_id: plugin_id.into(),
            instance_id,
            method: method.into(),
            payload,
        });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)?;
        Ok(StreamHandle {
            stream_id,
            receiver: rx,
            runtime: Arc::downgrade(self),
        })
    }

    /// Requests creation of a native plugin instance. The returned handle
    /// resolves to the encoded response; plugin-generated code decodes the
    /// [`InstanceId`] and calls [`Runtime::register_instance`].
    pub fn create_instance(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
        let envelope = Envelope::new(Frame::CreateInstance {
            call_id,
            plugin_id: plugin_id.into(),
            payload,
        });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)?;
        Ok(CallHandle {
            call_id,
            receiver: Some(rx),
            runtime: Arc::downgrade(self),
        })
    }

    /// Records an instance for later routing / bookkeeping.
    pub fn register_instance(&self, instance_id: InstanceId, plugin_id: impl Into<String>) {
        self.routing.register_instance(
            instance_id,
            InstanceEntry {
                plugin_id: plugin_id.into(),
            },
        );
    }

    /// Sends `DestroyInstance` and removes the local registration.
    pub fn destroy_instance(&self, instance_id: InstanceId) -> Result<(), IstmoError> {
        self.routing.remove_instance(instance_id);
        let envelope = Envelope::new(Frame::DestroyInstance { instance_id });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Emits [`Frame::ReleaseNativeHandle`] so the native side can free the
    /// object backing `handle_id`. Fire-and-forget; the native side must
    /// treat unknown ids as no-ops.
    ///
    /// Called by [`crate::native_handle::NativeHandle`] on drop; plugins
    /// generally do not need to invoke it directly.
    pub fn release_native_handle(&self, handle_id: NativeHandleId) -> Result<(), IstmoError> {
        let envelope = Envelope::new(Frame::ReleaseNativeHandle { handle_id });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Cancels an in-flight call by removing its local receiver and sending
    /// a `Cancel` frame to the native side.
    pub fn cancel_call(&self, call_id: CallId) -> Result<(), IstmoError> {
        self.routing.remove_pending(call_id.get());
        let envelope = Envelope::new(Frame::Cancel { call_id });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Cancels an open stream. Streams share the numeric id with the
    /// initiating call, so the same `Cancel` frame terminates them.
    pub fn cancel_stream(&self, stream_id: StreamId) -> Result<(), IstmoError> {
        self.routing.remove_pending(stream_id.get());
        let envelope = Envelope::new(Frame::Cancel {
            call_id: CallId(stream_id.get()),
        });
        self.outbound
            .send(envelope)
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Routes an inbound envelope. Called by the platform backend for each
    /// frame received from native.
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
            Frame::CreateInstance { .. }
            | Frame::DestroyInstance { .. }
            | Frame::ReleaseNativeHandle { .. } => {
                tracing::warn!("dropped inbound frame with outbound-only variant");
                Ok(())
            }
        }
    }

    /// Registers a hosted plugin dispatcher. Called by the `istmo::runtime!`
    /// macro for every trait in the `hosts:` or `services:` section.
    pub fn register_host<D: Dispatch>(self: &Arc<Self>, dispatcher: D) {
        let plugin_id = dispatcher.plugin_id();
        let dispatcher: Arc<dyn Dispatch> = Arc::new(dispatcher);
        dispatcher.runtime_attached(Arc::downgrade(self));
        lock(&self.hosts).insert(plugin_id, dispatcher);
    }

    /// Records that this process expects a plugin id on the client side.
    /// `acquire`-style helpers fail with [`IstmoError::PluginNotDeclared`]
    /// for ids not in this set.
    pub fn declare_plugin(&self, plugin_id: &'static str) {
        lock(&self.declared_plugins).insert(plugin_id);
    }

    /// Returns `true` when `plugin_id` was previously passed to
    /// [`Self::declare_plugin`].
    #[must_use]
    pub fn is_plugin_declared(&self, plugin_id: &str) -> bool {
        lock(&self.declared_plugins).contains(plugin_id)
    }

    /// Turns strict declaration checking on. The `istmo::runtime!` macro
    /// enables it after adding every `plugins:` entry so that late
    /// `acquire()` calls for undeclared ids fail fast.
    pub fn set_enforce_declarations(&self, enforce: bool) {
        self.enforce_declarations.store(enforce, Ordering::Relaxed);
    }

    /// Validates that `plugin_id` is declared for this process. When
    /// enforcement is off (the default for mocks), always returns `Ok(())`.
    ///
    /// # Errors
    /// Returns [`IstmoError::PluginNotDeclared`] when enforcement is on and
    /// the id is missing from the declared set.
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
        lock(&self.cancelled_hosted).insert(call_id);
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
        let runtime = Arc::clone(self);
        std::thread::spawn(move || {
            let outcome = pollster::block_on(async {
                dispatcher.dispatch(instance_id, &method, &payload).await
            });
            let mut cancelled = lock(&runtime.cancelled_hosted);
            if cancelled.remove(&call_id) {
                tracing::debug!(?call_id, "hosted call cancelled; discarding response");
                return;
            }
            drop(cancelled);
            let result: CallResult = match outcome {
                Ok(Outcome::Ok(bytes)) => Ok(bytes),
                Ok(Outcome::DomainError(bytes)) => Err(bytes),
                Err(err) => {
                    tracing::error!(?err, ?call_id, "hosted dispatch failed");
                    Err(encode_dispatch_error_string(&err.to_string()))
                }
            };
            runtime.send_respond(call_id, result);
        });
    }

    fn send_respond(&self, call_id: CallId, result: CallResult) {
        // Local short-circuit: if a same-runtime caller is waiting on this
        // call id, deliver the response into its routing entry without a
        // wire round-trip. Otherwise the response is destined for a remote
        // consumer (Kotlin / iOS) and goes out through the pump.
        if self.routing.has_pending(call_id.get()) {
            if let Err(err) = self.routing.deliver_response(call_id, result) {
                tracing::warn!(?err, "local Respond delivery failed");
            }
            return;
        }
        let envelope = Envelope::new(Frame::Respond { call_id, result });
        if let Err(err) = self.outbound.send(envelope) {
            tracing::warn!(
                ?err,
                "failed to send Respond frame; outbound channel closed"
            );
        }
    }

    /// Publishes `payload` to the latest-value slot named `channel`, creating
    /// the slot on first use.
    ///
    /// Intended for the platform backend to forward events whose consumer may
    /// not have subscribed yet (application lifecycle transitions being the
    /// canonical case). A late [`crate::early_events::LatestValueSlot::subscribe`]
    /// caller immediately observes the last value published here.
    pub fn publish_early_latest(&self, channel: &str, payload: Vec<u8>) {
        self.early_events.latest_slot(channel).publish(payload);
    }

    /// Publishes `payload` to the pre-main queue named `channel`, creating the
    /// queue with the given `capacity` on first use. When the queue already
    /// exists, its previously-configured capacity is preserved.
    ///
    /// Intended for launch-intent-style events (deep links, launch push
    /// notifications) that may be produced before the plugin subscribes.
    pub fn publish_early_queue(&self, channel: &str, capacity: usize, payload: Vec<u8>) {
        self.early_events.queue(channel, capacity).publish(payload);
    }

    /// Cancels every in-flight call / stream. Intended for platform
    /// teardown (Activity destroyed, application terminating).
    pub fn shutdown(&self) {
        let count = self.routing.cancel_all_pending();
        if count > 0 {
            tracing::info!(count, "istmo runtime shutdown cancelled pending operations");
        }
    }
}

/// Encodes a dispatch error as a bincode string so the client at least sees a
/// human-readable domain error. This is a fallback for infrastructure failures
/// (unknown method, decode failure); genuine domain errors use their own type.
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
        cancelled_hosted: Mutex::new(HashSet::new()),
    });
    RuntimeInit {
        runtime,
        outbound: outbound_rx,
    }
}

/// Handle to an in-flight unary call. Resolves to the encoded response, and
/// automatically sends a `Cancel` frame if dropped before it completes.
#[derive(Debug)]
pub struct CallHandle {
    call_id: CallId,
    receiver: Option<oneshot::Receiver<CallResult>>,
    runtime: Weak<Runtime>,
}

impl CallHandle {
    /// The call id assigned by the runtime.
    #[must_use]
    pub const fn call_id(&self) -> CallId {
        self.call_id
    }

    /// Blocks the current thread until the response arrives. Intended for
    /// tests and mock backends.
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
            // A closed outbound channel means the runtime is going away and
            // any Cancel would race with shutdown — no point surfacing the error.
            drop(rt.cancel_call(self.call_id));
        }
    }
}

/// Handle to an open stream. Automatically sends a `Cancel` frame on drop,
/// which is idempotent on the native side.
#[derive(Debug)]
pub struct StreamHandle {
    stream_id: StreamId,
    receiver: FlumeReceiver<StreamMessage>,
    runtime: Weak<Runtime>,
}

impl StreamHandle {
    /// The stream id assigned by the runtime.
    #[must_use]
    pub const fn stream_id(&self) -> StreamId {
        self.stream_id
    }

    /// Blocks until the next stream message arrives.
    pub fn recv(&self) -> Result<StreamMessage, IstmoError> {
        self.receiver.recv().map_err(|_| IstmoError::ChannelClosed)
    }

    /// Awaits the next stream message.
    pub async fn recv_async(&self) -> Result<StreamMessage, IstmoError> {
        self.receiver
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)
    }

    /// Non-blocking read of the next available message.
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
