//! Server-side plugin dispatch surface.
//!
//! `#[istmo::plugin]` generates a `Hosted<T: Trait>` struct implementing
//! [`Dispatch`] for every trait declared as a `hosts:` entry in the process's
//! [`crate::runtime`] configuration. The runtime routes inbound
//! [`crate::protocol::Frame::Call`] frames to the dispatcher by
//! [`Dispatch::plugin_id`].
//!
//! A dispatcher decodes the method arguments, calls the concrete `impl Trait
//! for TImpl` method, and returns an [`Outcome`] — the transport layer wraps
//! it in a [`crate::protocol::Frame::Respond`]. Domain errors declared on the
//! trait (`Result<T, E>`) travel through [`Outcome::DomainError`]; they are
//! opaque bytes at this layer.
//!
//! All decode / encode failures surface as typed [`DispatchError`] variants —
//! never as panics.

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

/// Cooperative cancellation token handed to a hosted dispatcher on every
/// [`Dispatch::dispatch`] invocation.
///
/// The runtime trips the token when it observes an inbound
/// [`crate::protocol::Frame::Cancel`] for the call. Impls either poll
/// [`Self::is_cancelled`] between logical steps or `await` [`Self::cancelled`]
/// alongside their real work and bail early — the response is discarded
/// whether they observe the token or not, so cooperative checking is a
/// latency / resource optimisation, not a correctness requirement.
///
/// The token also carries an async wake path (internally backed by a flume
/// channel whose sender is dropped on cancel) so adapters can bridge cancel
/// signals into other primitives without polling in a hot loop.
#[derive(Clone, Debug)]
pub struct CancelToken {
    inner: Arc<CancelInner>,
}

#[derive(Debug)]
struct CancelInner {
    flag: AtomicBool,
    /// Present until [`CancelToken::cancel`] runs, then taken and dropped so
    /// every clone of `rx` disconnects and wakes.
    tx: Mutex<Option<flume::Sender<()>>>,
    rx: flume::Receiver<()>,
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

impl CancelToken {
    /// Construct a fresh, not-yet-cancelled token.
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

    /// `true` once the runtime has observed the corresponding
    /// [`crate::protocol::Frame::Cancel`].
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        self.inner.flag.load(Ordering::Acquire)
    }

    /// Trip the token. Public so adapters that layer another cancellation
    /// primitive on top can propagate the signal. Idempotent — subsequent
    /// calls are no-ops.
    pub fn cancel(&self) {
        self.inner.flag.store(true, Ordering::Release);
        drop(lock(&self.inner.tx).take());
    }

    /// Async wait for cancellation. Resolves immediately if the token is
    /// already tripped; otherwise resolves the first time [`Self::cancel`]
    /// runs (which drops the internal sender and disconnects the receiver).
    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let _ = self.inner.rx.recv_async().await;
    }
}

/// Terminal outcome of a single hosted method call.
///
/// The transport turns this into `Frame::Respond { result: Ok / Err }`
/// for unary methods; for [`Self::StreamOpened`] the runtime pumps
/// [`crate::protocol::Frame::Event`] frames from the attached receiver
/// and emits a [`crate::protocol::Frame::StreamEnd`] when it disconnects.
#[derive(Debug, Clone)]
pub enum Outcome {
    /// The method returned a successful value; the bytes are the encoded `T`.
    Ok(Vec<u8>),
    /// The trait signature was `Result<T, E>` and the method returned `Err`;
    /// the bytes are the encoded `E`.
    DomainError(Vec<u8>),
    /// The method is a stream; each element received on the receiver
    /// (bytes already encoded on the host side) is forwarded to the
    /// caller as a `Frame::Event`. When the sender is dropped the
    /// runtime emits `Frame::StreamEnd { Complete }`.
    StreamOpened(flume::Receiver<Vec<u8>>),
}

impl PartialEq for Outcome {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Ok(a), Self::Ok(b)) | (Self::DomainError(a), Self::DomainError(b)) => a == b,
            // `flume::Receiver` has no meaningful equality — a stream
            // outcome only compares equal to itself when the tests take
            // that shortcut; the runtime never uses this comparison.
            _ => false,
        }
    }
}

impl Eq for Outcome {}

/// Failures the dispatch layer surfaces to the runtime. Never a panic path.
#[derive(Debug)]
pub enum DispatchError {
    /// Decoding a method's argument tuple failed.
    Decode(CodecError),
    /// Encoding the return value or domain error failed.
    Encode(CodecError),
    /// The `method` string on the Call frame is not known to this dispatcher.
    UnknownMethod(String),
    /// The method is declared on the trait but its concrete implementation is
    /// not yet available on this platform (typically stream methods on the
    /// hosted side, which the M-current codegen leaves as a TODO).
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

/// A future returned by [`Dispatch::dispatch`]. Boxed to let the trait be
/// object-safe — the runtime holds dispatchers behind `Arc<dyn Dispatch>`.
pub type DispatchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Outcome, DispatchError>> + Send + 'a>>;

/// Marker trait implemented by every generated plugin client and host so that
/// generic runtime helpers can name the wire plugin id without threading a
/// separate `&'static str` argument.
pub trait Plugin {
    /// Wire identifier of the plugin.
    const PLUGIN_ID: &'static str;
}

/// Server-side dispatch surface for a single plugin trait.
///
/// Generated by `#[istmo::plugin]`. The runtime treats every implementer
/// uniformly; each concrete dispatcher owns its `impl Trait for TImpl`
/// instance.
pub trait Dispatch: Send + Sync + 'static {
    /// The wire plugin id this dispatcher handles.
    fn plugin_id(&self) -> &'static str;

    /// Called once at registration time by
    /// [`crate::runtime::Runtime::register_host`], handing the dispatcher a
    /// weak reference to the runtime it lives in.
    ///
    /// Most dispatchers ignore this hook — the default is a no-op. Service
    /// adapters use it to reach the runtime later, when spawning the
    /// concrete service task on an inbound `on_start` call.
    fn runtime_attached(&self, runtime: Weak<Runtime>) {
        let _ = runtime;
    }

    /// Handle a single Call frame. The runtime provides the instance id
    /// (`None` for stateless plugins), the method name, the argument payload
    /// bytes and a [`CancelToken`] the caller can poll for cooperative
    /// cancellation; the dispatcher decodes the arguments, invokes the
    /// concrete impl and returns an [`Outcome`].
    ///
    /// `cancel` trips when the runtime observes an inbound
    /// [`crate::protocol::Frame::Cancel`] for this call. Impls that hold long
    /// futures should poll [`CancelToken::is_cancelled`] between logical steps
    /// and short-circuit; the runtime discards the response either way, so
    /// checking is an optimisation.
    fn dispatch<'a>(
        &'a self,
        instance_id: Option<InstanceId>,
        method: &'a str,
        payload: &'a [u8],
        cancel: CancelToken,
    ) -> DispatchFuture<'a>;
}
