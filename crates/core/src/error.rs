//! Error types surfaced by the runtime and codec.

use core::fmt;

use crate::protocol::{CallId, InstanceId, StreamId};

/// Everything the runtime can return through a fallible plugin call.
///
/// Domain errors raised by a plugin implementation are carried as
/// [`IstmoError::PluginError`] with the bincode-encoded payload — the
/// caller is expected to decode it against the plugin's declared error
/// type.
#[derive(Debug)]
pub enum IstmoError {
    /// The process-global [`Runtime`](crate::runtime::Runtime) has not
    /// been initialised.
    RuntimeNotStarted,
    /// The process-global runtime is already initialised. Attempts to
    /// start a second runtime in the same process are rejected.
    RuntimeAlreadyStarted,
    /// The wire codec failed. Wraps the underlying [`CodecError`].
    Codec(CodecError),
    /// No plugin is registered under this id.
    UnknownPlugin(String),
    /// The referenced instance no longer exists or was never created.
    UnknownInstance(InstanceId),
    /// The response references a call id the routing table doesn't know
    /// about — usually a late response after a cancel.
    UnknownCallId(CallId),
    /// The event or stream-end references an unknown stream id.
    UnknownStreamId(StreamId),
    /// A queue or oneshot channel closed before the value could be
    /// delivered. Typically means the runtime is shutting down.
    ChannelClosed,
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
    /// A plugin-authored domain error, bincode-encoded against the
    /// plugin's declared error type.
    PluginError {
        /// Encoded domain-error payload.
        bytes: Vec<u8>,
    },
    /// The client was constructed for a plugin the runtime doesn't know
    /// about. Add the client to the `plugins:` list of
    /// [`runtime!`](../../istmo_macros/macro.runtime.html).
    PluginNotDeclared(&'static str),
    /// The plugin is declared but no native binding is registered — the
    /// platform side hasn't installed its dispatcher yet.
    MissingNativePlugin(&'static str),
}

impl fmt::Display for IstmoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::RuntimeNotStarted => f.write_str("istmo runtime not started"),
            Self::RuntimeAlreadyStarted => f.write_str("istmo runtime already started"),
            Self::Codec(err) => write!(f, "codec error: {err}"),
            Self::UnknownPlugin(id) => write!(f, "unknown plugin id: {id}"),
            Self::UnknownInstance(id) => write!(f, "unknown instance id: {id}"),
            Self::UnknownCallId(id) => write!(f, "unknown call id: {id}"),
            Self::UnknownStreamId(id) => write!(f, "unknown stream id: {id}"),
            Self::ChannelClosed => f.write_str("istmo channel closed unexpectedly"),
            Self::ProtocolVersionMismatch { expected, got } => write!(
                f,
                "protocol version mismatch: expected {expected}, got {got}",
            ),
            Self::RoutingMismatch(kind) => write!(f, "routing mismatch: {kind}"),
            Self::PluginError { bytes } => {
                write!(f, "plugin domain error ({} bytes)", bytes.len())
            }
            Self::PluginNotDeclared(id) => {
                write!(f, "plugin `{id}` not declared in istmo::runtime!")
            }
            Self::MissingNativePlugin(id) => {
                write!(f, "declared plugin `{id}` has no native binding")
            }
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

impl From<CodecError> for IstmoError {
    fn from(err: CodecError) -> Self {
        Self::Codec(err)
    }
}

/// Bincode encode / decode failures.
///
/// Wrapped by [`IstmoError::Codec`] when surfaced through the runtime.
#[derive(Debug)]
pub enum CodecError {
    /// Encoding a value to bytes failed.
    Encode(bincode::error::EncodeError),
    /// Decoding a value from bytes failed.
    Decode(bincode::error::DecodeError),
}

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Encode(err) => write!(f, "encode: {err}"),
            Self::Decode(err) => write!(f, "decode: {err}"),
        }
    }
}

impl std::error::Error for CodecError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Encode(err) => Some(err),
            Self::Decode(err) => Some(err),
        }
    }
}

impl From<bincode::error::EncodeError> for CodecError {
    fn from(err: bincode::error::EncodeError) -> Self {
        Self::Encode(err)
    }
}

impl From<bincode::error::DecodeError> for CodecError {
    fn from(err: bincode::error::DecodeError) -> Self {
        Self::Decode(err)
    }
}
