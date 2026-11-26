//! Error types surfaced by the runtime and codec.

use core::fmt;

use crate::protocol::{CallId, StreamId};

/// Everything the runtime can return through a fallible plugin call.
///
/// Domain errors raised by a plugin implementation are carried as
/// [`IstmoError::PluginError`] with the wire-encoded payload — the
/// caller is expected to decode it against the plugin's declared error
/// type.
#[derive(Debug)]
#[non_exhaustive]
pub enum IstmoError {
    /// The process-global [`Runtime`](crate::runtime::Runtime) has not
    /// been initialised.
    RuntimeNotStarted,
    /// The process-global runtime is already initialised. Attempts to
    /// start a second runtime in the same process are rejected.
    RuntimeAlreadyStarted,
    /// [`Runtime::shutdown`](crate::runtime::Runtime::shutdown) ran; the
    /// runtime no longer accepts new calls, streams, instances or
    /// notifications.
    RuntimeShutDown,
    /// The wire codec failed. Wraps the underlying [`CodecError`].
    Codec(CodecError),
    /// The response references a call id the routing table doesn't know
    /// about — usually a late response after a cancel.
    UnknownCallId(CallId),
    /// The event or stream-end references an unknown stream id.
    UnknownStreamId(StreamId),
    /// A queue or oneshot channel closed before the value could be
    /// delivered. Typically means the runtime is shutting down.
    ChannelClosed,
    /// The outbound channel was full on `wasm32`, where a synchronous
    /// send cannot wait for room, so the frame was not sent. Frames that
    /// must not be lost (a cancel, a handle release) queue instead.
    OutboundFull,
    /// The peer's [`PROTOCOL_VERSION`](crate::protocol::PROTOCOL_VERSION)
    /// does not match this runtime's.
    ProtocolVersionMismatch {
        /// Version this runtime expects.
        expected: u16,
        /// Version observed on the wire.
        got: u16,
    },
    /// An outcome came back on the wrong routing lane (e.g. a stream
    /// event landed on a unary call registration).
    RoutingMismatch(&'static str),
    /// A plugin-authored domain error, wire-encoded against the
    /// plugin's declared error type.
    PluginError {
        /// Encoded domain-error payload.
        bytes: Vec<u8>,
    },
    /// The client was constructed for a plugin the runtime doesn't know
    /// about. Add the client to the `plugins:` list of
    /// [`runtime!`](../../istmo_macros/macro.runtime.html).
    PluginNotDeclared(&'static str),
    /// No frame sink with this raw id is open in this runtime: it was
    /// closed, or opened by another runtime or process.
    UnknownFrameSink(u64),
    /// The frame sink exists but carries another metadata type than the
    /// one asked for.
    FrameSinkTypeMismatch {
        /// The sink's raw id.
        id: u64,
        /// The metadata type the caller asked for.
        expected: &'static str,
        /// The metadata type the sink carries.
        found: &'static str,
    },
    /// A frame sink id was passed to a plugin hosted in another process,
    /// where no producer can reach the sink.
    FrameSinkNotShareable(&'static str),
}

impl fmt::Display for IstmoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotStarted => f.write_str("istmo runtime not started"),
            Self::RuntimeAlreadyStarted => f.write_str("istmo runtime already started"),
            Self::RuntimeShutDown => f.write_str("istmo runtime shut down"),
            Self::Codec(err) => write!(f, "codec error: {err}"),
            Self::UnknownCallId(id) => write!(f, "unknown call id: {id}"),
            Self::UnknownStreamId(id) => write!(f, "unknown stream id: {id}"),
            Self::ChannelClosed => f.write_str("istmo channel closed unexpectedly"),
            Self::OutboundFull => f.write_str("istmo outbound channel full; frame dropped"),
            Self::ProtocolVersionMismatch { expected, got } => write!(
                f,
                "protocol version mismatch: expected {expected}, got {got}",
            ),
            Self::RoutingMismatch(kind) => write!(f, "routing mismatch: {kind}"),
            Self::PluginError { bytes } => write!(
                f,
                "plugin domain error ({} bytes; decode it with `IstmoError::domain_error`)",
                bytes.len()
            ),
            Self::PluginNotDeclared(id) => {
                write!(f, "plugin `{id}` not declared in istmo::runtime!")
            }
            Self::UnknownFrameSink(id) => {
                write!(
                    f,
                    "no frame sink {id:#018x} in this runtime (closed, or opened elsewhere)"
                )
            }
            Self::FrameSinkTypeMismatch {
                id,
                expected,
                found,
            } => write!(
                f,
                "frame sink {id:#018x} carries `{found}` metadata, not `{expected}`"
            ),
            Self::FrameSinkNotShareable(plugin) => write!(
                f,
                "plugin `{plugin}` runs in another process and cannot feed a frame sink"
            ),
        }
    }
}

impl std::error::Error for IstmoError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Codec(err) => Some(err),
            _ => None,
        }
    }
}

impl IstmoError {
    /// The plugin's domain error, decoded as `E`: `Some` for a
    /// [`PluginError`](Self::PluginError) whose bytes are an `E`.
    ///
    /// ```
    /// # use istmo_core::{IstmoError, codec};
    /// let err = IstmoError::PluginError { bytes: codec::encode(&7u32).unwrap() };
    /// assert_eq!(err.domain_error::<u32>(), Some(7));
    /// assert_eq!(IstmoError::ChannelClosed.domain_error::<u32>(), None);
    /// ```
    #[must_use]
    pub fn domain_error<E: crate::message::Message>(&self) -> Option<E> {
        match self {
            Self::PluginError { bytes } => crate::codec::decode_exact::<E>(bytes).ok(),
            _ => None,
        }
    }
}

impl From<CodecError> for IstmoError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

/// Wire encode / decode failures.
///
/// Wrapped by [`IstmoError::Codec`] when surfaced through the runtime.
#[derive(Debug)]
#[non_exhaustive]
pub enum CodecError {
    /// Encoding a value to bytes failed.
    Encode(wincode::WriteError),
    /// Decoding a value from bytes failed.
    Decode(wincode::ReadError),
    /// A value decoded with bytes left over after it.
    TrailingBytes {
        /// How many bytes were left.
        extra: usize,
    },
    /// Input longer than any value of its kind may be.
    TooLong {
        /// Input length.
        len: usize,
        /// Most bytes the kind may take.
        cap: usize,
    },
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(err) => write!(f, "encode: {err}"),
            Self::Decode(err) => write!(f, "decode: {err}"),
            Self::TrailingBytes { extra } => write!(f, "decode: {extra} trailing bytes"),
            Self::TooLong { len, cap } => write!(f, "decode: {len} bytes, past the {cap} allowed"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(err) => Some(err),
            Self::Decode(err) => Some(err),
            Self::TrailingBytes { .. } | Self::TooLong { .. } => None,
        }
    }
}

impl From<wincode::WriteError> for CodecError {
    fn from(err: wincode::WriteError) -> Self {
        Self::Encode(err)
    }
}

impl From<wincode::ReadError> for CodecError {
    fn from(err: wincode::ReadError) -> Self {
        Self::Decode(err)
    }
}
