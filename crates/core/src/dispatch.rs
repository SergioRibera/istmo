//! Contracts implemented by plugin hosts and consumed by the runtime.
//!
//! - [`Plugin`] is the tiny marker every generated client/host pair
//!   carries so the runtime can address it by id.
//! - [`Dispatch`] is the object-safe entry point the runtime calls when
//!   a [`Frame::Call`](crate::protocol::Frame::Call) arrives.
//! - [`Outcome`] is what a dispatcher hands back.
//! - [`CancelToken`] is the cooperative-cancellation signal threaded
//!   through every dispatch.

use core::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::Weak;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::error::CodecError;
use crate::protocol::InstanceId;
use crate::runtime::Runtime;
use crate::sync::lock;

/// Cooperative-cancellation signal shared between a caller and a host.
///
/// Cloning a token yields another handle to the same underlying signal;
/// calling [`cancel`](Self::cancel) on any clone flips the shared flag
/// and wakes every task currently awaiting [`cancelled`](Self::cancelled).
///
/// The token exposes both a synchronous poll ([`is_cancelled`](Self::is_cancelled))
/// and an async wait ([`cancelled`](Self::cancelled)) so plugin authors
/// can pick whichever fits their concurrency model.
#[derive(Clone, Debug)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug)]
struct CancelInner {
    flag: AtomicBool,
    tx: Mutex<Option<flume::Sender<()>>>,
    rx: flume::Receiver<()>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// Build a fresh, un-cancelled token.
    #[must_use]
    pub fn new() -> Self {
        let (tx, rx) = flume::bounded(1);
        Self {
            inner: Arc::new(CancelInner {
                flag: AtomicBool::new(false),
                tx: Mutex::new(Some(tx)),
                rx,
            }),
        }
    }

    /// Return `true` once any clone of this token has been cancelled.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::Acquire)
    }

    /// Signal cancellation.
    ///
    /// Idempotent: subsequent calls are no-ops.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::Release);
        drop(lock(&self.inner.tx).take());
    }

    /// Resolve as soon as the token is cancelled.
    ///
    /// Returns immediately if the token has already been cancelled.
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let _ = self.inner.rx.recv_async().await;
    }
}

/// Result of dispatching a single [`Frame::Call`](crate::protocol::Frame::Call).
///
/// Unary methods produce [`Outcome::Ok`] or [`Outcome::DomainError`];
/// stream-returning methods produce [`Outcome::StreamOpened`], and the
/// runtime pumps the receiver's items out as
/// [`Frame::Event`](crate::protocol::Frame::Event) frames.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// Unary success. Bytes are a wire-encoded return value.
    Ok(Vec<u8>),
    /// Unary domain failure. Bytes are a wire-encoded plugin-declared
    /// error type.
    DomainError(Vec<u8>),
    /// Stream successfully opened. Each `Ok` item read from the receiver
    /// is forwarded to the caller as a
    /// [`Frame::Event`](crate::protocol::Frame::Event). An `Err` item
    /// ends the stream with
    /// [`StreamEndReason::Error`](crate::protocol::StreamEndReason::Error)
    /// carrying its bytes, and nothing after it is read; dropping the
    /// sender end closes the stream cleanly.
    StreamOpened(flume::Receiver<HostedStreamItem>),
}

/// One item a hosted stream hands the runtime: a wire-encoded item, or
/// a wire-encoded domain error that ends the stream.
pub type HostedStreamItem = Result<Vec<u8>, Vec<u8>>;

impl PartialEq for Outcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ok(a), Self::Ok(b)) | (Self::DomainError(a), Self::DomainError(b)) => a == b,
            _ => false,
        }
    }
}

impl Eq for Outcome {}

/// Failure raised by a dispatcher before the plugin implementation is
/// even called (bad wire payload, unknown method, and so on).
#[derive(Debug)]
#[non_exhaustive]
pub enum DispatchError {
    /// The wire payload could not be decoded against the method's
    /// declared arguments.
    Decode(CodecError),
    /// The plugin's return value could not be re-encoded.
    Encode(CodecError),
    /// The dispatcher does not implement the requested method name.
    UnknownMethod(String),
    /// A codegen stub is present but the concrete implementation hasn't
    /// been supplied yet.
    Unimplemented(&'static str),
    /// The OS refused a thread needed to serve the call (a hosted call,
    /// a stream encoder or pump, a service body or its cancel watcher).
    Spawn(std::io::Error),
    /// The method refuses callers of this origin (a method taking a
    /// frame sink only serves this process's own Rust code).
    OriginRefused {
        /// The refused method.
        method: String,
        /// Who called.
        origin: CallOrigin,
    },
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "dispatch decode: {err}"),
            Self::Encode(err) => write!(f, "dispatch encode: {err}"),
            Self::UnknownMethod(name) => write!(f, "unknown method: {name}"),
            Self::Unimplemented(name) => write!(f, "unimplemented method: {name}"),
            Self::Spawn(err) => write!(f, "could not spawn a dispatch thread: {err}"),
            Self::OriginRefused { method, origin } => {
                write!(f, "method `{method}` refuses {origin:?} callers")
            }
        }
    }
}

impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(err) | Self::Encode(err) => Some(err),
            Self::Spawn(err) => Some(err),
            Self::UnknownMethod(_) | Self::Unimplemented(_) | Self::OriginRefused { .. } => None,
        }
    }
}

/// Where a hosted request entered the runtime. See [`Dispatch::admits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CallOrigin {
    /// This process's own Rust code ([`Runtime::call`] and friends).
    Local,
    /// The platform peer of this runtime (Kotlin / Swift in the same
    /// process), through [`Runtime::dispatch_inbound`].
    Peer,
    /// A runtime in another process (Android `:remote`, a desktop
    /// helper), through [`Runtime::inject_wire_envelope`].
    Remote,
}

/// Boxed future returned by [`Dispatch::dispatch`].
pub type DispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Outcome, DispatchError>> + Send + 'a>>;

/// Hint that lets the runtime pick how it runs a host's dispatch future.
///
/// Dispatchers declare this through [`Dispatch::mode`]. The runtime reads
/// the hint on the first call and keeps that scheduling shape for the
/// lifetime of the dispatcher.
///
/// Trade-off summary:
///
/// - [`Inline`](Self::Inline): fastest, strictest. The future runs on the
///   caller's stack. The dispatcher **must not block**, hold a lock across
///   the boundary, re-enter the runtime recursively from the same call, or
///   await anything that requires another executor to make progress.
///   Breaking that stalls the caller. Suits pure computation (parsers,
///   formatters, state machines).
/// - [`OnCallerExecutor`](Self::OnCallerExecutor): the future runs as a
///   child of the caller's executor. `.await` points yield normally and
///   other work on that executor can interleave. The dispatcher must not
///   block the thread between await points.
/// - [`Isolated`](Self::Isolated): the future runs on a dedicated OS
///   thread the runtime spawns for the call. The dispatcher is free to
///   block, call synchronous platform SDKs, hold locks, or re-enter the
///   runtime. This is the safe default.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DispatchMode {
    /// Poll the dispatch future on the caller's thread with the caller's
    /// waker. No thread spawn, no cross-task handoff.
    Inline,
    /// Chain the dispatch future into the caller's executor. No thread
    /// spawn; `.await` yields back to the caller's executor.
    OnCallerExecutor,
    /// Dedicate a fresh OS thread to the dispatch future. The runtime
    /// picks this when a dispatcher may block or re-enter.
    Isolated,
}

/// Marker every generated plugin client/host pair implements.
///
/// The associated [`PLUGIN_ID`](Self::PLUGIN_ID) is the string the
/// runtime uses to route [`Frame::Call`](crate::protocol::Frame::Call)
/// frames to the right dispatcher.
pub trait Plugin {
    /// Stable string identifier for this plugin.
    ///
    /// Set by `#[istmo::plugin(id = "…")]`; must be unique within a
    /// running process.
    const PLUGIN_ID: &'static str;
}

/// Object-safe entry point the runtime uses to hand incoming calls to a
/// plugin implementation.
///
/// Generated `<Trait>Host` types implement this automatically; hand-
/// written plugins can implement it directly.
pub trait Dispatch: Send + Sync + 'static {
    /// Plugin id this dispatcher answers on.
    fn plugin_id(&self) -> &'static str;

    /// Scheduling shape the runtime picks for this dispatcher.
    ///
    /// The default is [`DispatchMode::Isolated`]: every call runs on its
    /// own OS thread, so the dispatch future is free to block, hold
    /// locks, or re-enter the runtime recursively. Dispatchers that
    /// never do any of those can override to [`DispatchMode::Inline`] or
    /// [`DispatchMode::OnCallerExecutor`] to avoid the per-call thread
    /// spawn.
    ///
    /// The runtime reads this on registration and keeps the answer for
    /// the dispatcher's lifetime, so it is effectively a per-type choice
    /// even though the method takes `&self`.
    fn mode(&self) -> DispatchMode {
        DispatchMode::Isolated
    }

    /// Called once when the dispatcher is registered with a runtime.
    ///
    /// Stateless dispatchers can ignore the callback; stateful ones
    /// typically stash the [`Weak<Runtime>`] so they can make outbound
    /// calls (e.g. re-enter the runtime to talk to another plugin).
    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = runtime;
    }

    /// Whether calls, instance creation/destruction and notifications
    /// from `origin` may reach this dispatcher.
    ///
    /// The runtime asks before every hosted request and answers refused
    /// calls with an error (notifications are dropped). The default
    /// admits everyone; hosts that hand out capabilities over the bus
    /// (a webview that runs script, a credential store) refuse
    /// [`CallOrigin::Remote`] so another process cannot drive them.
    fn admits(&self, origin: CallOrigin) -> bool {
        let _ = origin;
        true
    }

    /// Dispatch a single call.
    ///
    /// - `instance_id` — `Some` for stateful plugins, `None` for
    ///   stateless ones.
    /// - `method` — method name declared on the plugin trait.
    /// - `payload` — wire-encoded arguments tuple.
    /// - `cancel` — cooperative cancellation signal; fires when the
    ///   caller either drops its handle or sends
    ///   [`Frame::Cancel`](crate::protocol::Frame::Cancel).
    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a>;

    /// [`dispatch`](Self::dispatch), knowing who called. The runtime
    /// calls this one; the default ignores `origin`. Generated hosts
    /// override it to hand the origin to the implementation
    /// ([`HostCtx::origin`]) and to refuse frame-sink methods to
    /// callers outside this process.
    fn dispatch_from<'a>(
        &'a self,
        origin: CallOrigin,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let _ = origin;
        self.dispatch(instance_id, method, payload, cancel)
    }

    /// The typed in-process shortcut of this host, installed next to the
    /// wire path by [`Runtime::register_host`]: generated clients in this
    /// process call through it instead of encoding. `None` (the default,
    /// and what `#[plugin(shortcut = false)]` hosts answer) keeps every
    /// call on the wire path.
    #[doc(hidden)]
    fn typed_shortcut(self: Arc<Self>) -> Option<Arc<dyn std::any::Any + Send + Sync>> {
        None
    }

    /// Handle a [`Frame::CreateInstance`](crate::protocol::Frame::CreateInstance)
    /// for stateful plugins hosted in this process.
    ///
    /// Default returns [`DispatchError::UnknownMethod`] — stateless
    /// dispatchers never see this call. Stateful hosts override to
    /// decode `config_payload`, spin up per-instance state, and return
    /// the freshly-allocated [`InstanceId`] as wire-encoded bytes in
    /// [`Outcome::Ok`]. The runtime picks the id out of the response
    /// and wires it into the routing tables so subsequent
    /// [`dispatch`](Self::dispatch) calls carry `Some(instance_id)`.
    fn create_instance<'a>(
        &'a self,
        config_payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let _ = (config_payload, cancel);
        Box::pin(async { Err(DispatchError::UnknownMethod("__create_instance".to_owned())) })
    }

    /// Release per-instance state associated with `instance_id` after
    /// the caller has dropped its client. Default is a no-op — stateful
    /// hosts override to reap the entry from their instance map.
    fn destroy_instance(&self, instance_id: InstanceId) {
        let _ = instance_id;
    }
}

/// What a Rust host method can know about the call it serves: who made
/// it and the runtime it came through.
///
/// Declare an argument of this type on a plugin trait method (any
/// name; matched by the type's last path segment, like
/// [`CancelToken`]) and the generated host fills it in. It never
/// crosses the wire: Kotlin and Swift backends do not see it.
///
/// ```
/// use istmo_core::{FrameSinkId, HostCtx, NativeBuffer};
///
/// async fn start(ctx: HostCtx, frames: FrameSinkId<u32>) -> Result<(), istmo_core::IstmoError> {
///     let sender = ctx.frame_sender(frames)?;
///     std::thread::spawn(move || {
///         let _ = sender.push(1, NativeBuffer::from_bytes(vec![0u8; 4]));
///     });
///     Ok(())
/// }
/// # let _ = start;
/// ```
#[derive(Debug, Clone)]
pub struct HostCtx {
    runtime: Weak<Runtime>,
    origin: CallOrigin,
}

impl HostCtx {
    /// A context for a call from `origin` served on `runtime`.
    #[must_use]
    pub fn new(runtime: &Arc<Runtime>, origin: CallOrigin) -> Self {
        Self {
            runtime: Arc::downgrade(runtime),
            origin,
        }
    }

    /// The context generated hosts build from their stored runtime.
    #[doc(hidden)]
    #[must_use]
    pub const fn __from_weak(runtime: Weak<Runtime>, origin: CallOrigin) -> Self {
        Self { runtime, origin }
    }

    /// A context for a host that was never registered on a runtime
    /// (called directly, in a test): [`runtime`](Self::runtime) fails.
    #[must_use]
    pub const fn detached(origin: CallOrigin) -> Self {
        Self {
            runtime: Weak::new(),
            origin,
        }
    }

    /// Who made the call. The typed in-process shortcut always reports
    /// [`CallOrigin::Local`].
    #[must_use]
    pub const fn origin(&self) -> CallOrigin {
        self.origin
    }

    /// The runtime serving the call.
    ///
    /// # Errors
    ///
    /// [`IstmoError::RuntimeShutDown`] once the runtime is gone, or for
    /// a [`detached`](Self::detached) context.
    pub fn runtime(&self) -> Result<Arc<Runtime>, crate::IstmoError> {
        self.runtime
            .upgrade()
            .ok_or(crate::IstmoError::RuntimeShutDown)
    }

    /// The producer end of the frame sink `id`, opened by a consumer in
    /// this process.
    ///
    /// # Errors
    ///
    /// As [`Runtime::frame_sender`], plus
    /// [`IstmoError::RuntimeShutDown`](crate::IstmoError::RuntimeShutDown)
    /// when the runtime is gone.
    pub fn frame_sender<M: crate::Message + Send + 'static>(
        &self,
        id: crate::FrameSinkId<M>,
    ) -> Result<crate::FrameSender<M>, crate::IstmoError> {
        self.runtime()?.frame_sender(id)
    }
}
