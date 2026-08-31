//! Typed error taxonomy for the istmo core layer.
//!
//! All errors are hand-implemented; the crate intentionally avoids
//! `thiserror` to keep its dependency footprint minimal.

use core::fmt;

use crate::protocol::{CallId, InstanceId, StreamId};

/// Top-level error surfaced by the core runtime.
#[derive(Debug)]
pub enum IstmoError {
    /// The process runtime has not been initialised yet.
    RuntimeNotStarted,
    /// The process runtime was initialised twice.
    RuntimeAlreadyStarted,
    /// Wire codec failed while (de)serialising a frame or payload.
    Codec(CodecError),
    /// No plugin is registered under the given identifier.
    UnknownPlugin(String),
    /// The instance id referenced does not exist.
    UnknownInstance(InstanceId),
    /// No pending call routes to this id (already responded, cancelled or spurious).
    UnknownCallId(CallId),
    /// No active stream routes to this id.
    UnknownStreamId(StreamId),
    /// A required channel was closed while a send/receive was in flight.
    ChannelClosed,
    /// The peer advertised a protocol version this build does not understand.
    ProtocolVersionMismatch { expected: u16, got: u16 },
    /// Attempted to route a frame to the wrong kind of receiver
    /// (e.g. a `Respond` frame landing on a stream id).
    RoutingMismatch(&'static str),
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

/// Wire codec failures.
#[derive(Debug)]
pub enum CodecError {
    /// Encoding a value into bytes failed.
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
