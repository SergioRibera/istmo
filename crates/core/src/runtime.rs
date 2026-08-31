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

use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock, Weak};
use std::task::{Context, Poll};

use flume::{Receiver as FlumeReceiver, Sender as FlumeSender, bounded};

use crate::early_events::EarlyEventStore;
use crate::error::IstmoError;
use crate::main_thread::{InlineMainThread, MainThread};
use crate::protocol::{CallId, Envelope, Frame, InstanceId, PROTOCOL_VERSION, StreamId};
use crate::routing::{CallResult, InstanceEntry, RoutingTables, StreamMessage};

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
#[derive(Debug)]
pub struct RuntimeInit {
    pub runtime: Arc<Runtime>,
    pub outbound: FlumeReceiver<Envelope>,
}

/// Process-scoped runtime. Never construct directly — use [`Runtime::init`]
/// or [`Runtime::mock`].
pub struct Runtime {
    outbound: FlumeSender<Envelope>,
    routing: Arc<RoutingTables>,
    early_events: Arc<EarlyEventStore>,
    main_thread: Arc<dyn MainThread>,
    next_id: AtomicU64,
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
    pub fn call(
        self: &Arc<Self>,
        plugin_id: impl Into<String>,
        instance_id: Option<InstanceId>,
        method: impl Into<String>,
        payload: Vec<u8>,
    ) -> Result<CallHandle, IstmoError> {
        let call_id = CallId(self.next_id());
        let rx = self.routing.register_call(call_id);
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
    pub fn dispatch_inbound(&self, envelope: Envelope) -> Result<(), IstmoError> {
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
            Frame::Call { .. }
            | Frame::Cancel { .. }
            | Frame::CreateInstance { .. }
            | Frame::DestroyInstance { .. } => {
                tracing::warn!("dropped inbound frame with outbound-only variant");
                Ok(())
            }
        }
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

fn build(config: RuntimeConfig) -> RuntimeInit {
    let (outbound_tx, outbound_rx) = bounded(config.outbound_capacity);
    let runtime = Arc::new(Runtime {
        outbound: outbound_tx,
        routing: Arc::new(RoutingTables::new()),
        early_events: Arc::new(EarlyEventStore::new()),
        main_thread: config.main_thread,
        next_id: AtomicU64::new(1),
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
