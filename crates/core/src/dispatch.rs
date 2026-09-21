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
    /// Unary success. Bytes are a bincode-encoded return value.
    Ok(Vec<u8>),
    /// Unary domain failure. Bytes are a bincode-encoded plugin-declared
    /// error type.
    DomainError(Vec<u8>),
    /// Stream successfully opened. Each item read from the receiver is
    /// forwarded to the caller as a [`Frame::Event`](crate::protocol::Frame::Event);
    /// dropping the sender end closes the stream cleanly.
    StreamOpened(flume::Receiver<Vec<u8>>),
}

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
}

impl fmt::Display for DispatchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(err) => write!(f, "dispatch decode: {err}"),
            Self::Encode(err) => write!(f, "dispatch encode: {err}"),
            Self::UnknownMethod(name) => write!(f, "unknown method: {name}"),
            Self::Unimplemented(name) => write!(f, "unimplemented method: {name}"),
        }
    }
}

impl std::error::Error for DispatchError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Decode(err) | Self::Encode(err) => Some(err),
            Self::UnknownMethod(_) | Self::Unimplemented(_) => None,
        }
    }
}

/// Boxed future returned by [`Dispatch::dispatch`].
pub type DispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Outcome, DispatchError>> + Send + 'a>>;

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

    /// Called once when the dispatcher is registered with a runtime.
    ///
    /// Stateless dispatchers can ignore the callback; stateful ones
    /// typically stash the [`Weak<Runtime>`] so they can make outbound
    /// calls (e.g. re-enter the runtime to talk to another plugin).
    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = runtime;
    }

    /// Dispatch a single call.
    ///
    /// - `instance_id` — `Some` for stateful plugins, `None` for
    ///   stateless ones.
    /// - `method` — method name declared on the plugin trait.
    /// - `payload` — bincode-encoded arguments tuple.
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

    /// Handle a [`Frame::CreateInstance`](crate::protocol::Frame::CreateInstance)
    /// for stateful plugins hosted in this process.
    ///
    /// Default returns [`DispatchError::UnknownMethod`] — stateless
    /// dispatchers never see this call. Stateful hosts override to
    /// decode `config_payload`, spin up per-instance state, and return
    /// the freshly-allocated [`InstanceId`] as bincode-encoded bytes in
    /// [`Outcome::Ok`]. The runtime picks the id out of the response
    /// and wires it into the routing tables so subsequent
    /// [`dispatch`](Self::dispatch) calls carry `Some(instance_id)`.
    fn create_instance<'a>(
        &'a self,
        config_payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a> {
        let _ = (config_payload, cancel);
        Box::pin(async {
            Err(DispatchError::UnknownMethod(
                "__create_instance".to_owned(),
            ))
        })
    }

    /// Release per-instance state associated with `instance_id` after
    /// the caller has dropped its client. Default is a no-op — stateful
    /// hosts override to reap the entry from their instance map.
    fn destroy_instance(&self, instance_id: InstanceId) {
        let _ = instance_id;
    }
}
