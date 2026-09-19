//! Wire protocol: [`Envelope`], [`Frame`], typed identifiers and the
//! current [`PROTOCOL_VERSION`].
//!
//! Every message that crosses the FFI boundary is a bincode-encoded
//! [`Envelope`] carrying a single [`Frame`]. Version mismatches are
//! rejected at decode time by [`Envelope::from_wire_bytes`].

use core::fmt;

use bincode::{Decode, Encode};

/// Current on-wire protocol version.
///
/// Peers exchanging frames encoded with a different version are rejected
/// at decode time with [`IstmoError::ProtocolVersionMismatch`](crate::error::IstmoError::ProtocolVersionMismatch).
/// Bumped whenever a new [`Frame`] variant is introduced or an existing
/// one changes shape.
pub const PROTOCOL_VERSION: u16 = 4;

macro_rules! id_newtype {
    ($(#[$attr:meta])* $name:ident, $short:literal) => {
        $(#[$attr])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
        #[repr(transparent)]
        pub struct $name(pub u64);

        impl $name {
            /// Wrap a raw `u64` in this typed identifier.
            #[must_use]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            /// Return the underlying `u64`.
            #[must_use]
            pub const fn get(self) -> u64 {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", $short, self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}#{}", $short, self.0)
            }
        }

        impl From<u64> for $name {
            fn from(value: u64) -> Self {
                Self(value)
            }
        }
    };
}

id_newtype!(
    /// Identifier for a single request/response pair.
    ///
    /// Allocated by the caller side of the runtime and echoed in the
    /// matching [`Frame::Respond`].
    CallId,
    "call"
);
id_newtype!(
    /// Identifier for an active stream of events.
    ///
    /// Allocated when a stream-returning method is invoked; carried on
    /// every [`Frame::Event`] and closed by [`Frame::StreamEnd`].
    StreamId,
    "stream"
);
id_newtype!(
    /// Identifier for a stateful plugin instance created via
    /// [`Frame::CreateInstance`].
    ///
    /// Passed on subsequent [`Frame::Call`] frames to route methods to
    /// the correct instance. Released by [`Frame::DestroyInstance`].
    InstanceId,
    "instance"
);
id_newtype!(
    /// Wire representation of a [`NativeHandle`](crate::native_handle::NativeHandle).
    ///
    /// Opaque `u64` referring to a platform-owned object (for example a
    /// Kotlin `Credential`). Released by [`Frame::ReleaseNativeHandle`]
    /// when the corresponding `NativeHandle` is dropped on the Rust side.
    NativeHandleId,
    "native"
);

/// Reason a stream terminated.
///
/// Carried by [`Frame::StreamEnd`] to distinguish clean completion from
/// caller-initiated cancellation and provider-side failure.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum StreamEndReason {
    /// Producer finished normally.
    Complete,
    /// Consumer cancelled the subscription (dropped its handle or
    /// signalled a [`CancelToken`](crate::dispatch::CancelToken)).
    Cancelled,
    /// Producer errored out. The wrapped bytes are the bincode-encoded
    /// domain error emitted by the plugin.
    Error(Vec<u8>),
}

/// Retention policy for [`Frame::EarlyEvent`].
///
/// Controls how the runtime buffers events published before their first
/// subscriber attaches.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum EarlyEventKind {
    /// Keep only the most recent value. Late subscribers see the latest
    /// value first, then live updates.
    Latest,
    /// Keep a bounded FIFO. Oldest entries are dropped once `capacity`
    /// is exceeded; the queue drains into the first subscriber.
    Queue {
        /// Maximum entries retained before oldest is dropped.
        capacity: u32,
    },
}

/// Versioned wrapper around a [`Frame`] as it appears on the wire.
///
/// Encoded and decoded via [`Envelope::to_wire_bytes`] /
/// [`Envelope::from_wire_bytes`]. The `version` field is compared
/// against [`PROTOCOL_VERSION`] on decode and mismatches produce
/// [`IstmoError::ProtocolVersionMismatch`](crate::error::IstmoError::ProtocolVersionMismatch).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Envelope {
    /// Protocol version this envelope was encoded against.
    pub version: u16,
    /// The frame carried inside the envelope.
    pub frame: Frame,
}

impl Envelope {
    /// Build a new envelope stamped with the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub const fn new(frame: Frame) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            frame,
        }
    }

    /// Bincode-encode this envelope to a byte buffer suitable for
    /// hopping across FFI or a process boundary.
    ///
    /// # Errors
    ///
    /// Returns [`CodecError`](crate::error::CodecError) if bincode
    /// serialization fails.
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>, crate::error::CodecError> {
        crate::codec::encode(self)
    }

    /// Decode an envelope from its wire representation.
    ///
    /// # Errors
    ///
    /// - [`IstmoError::Codec`](crate::error::IstmoError::Codec) if the
    ///   bytes cannot be decoded.
    /// - [`IstmoError::ProtocolVersionMismatch`](crate::error::IstmoError::ProtocolVersionMismatch)
    ///   if the envelope was encoded against a different version of the
    ///   protocol.
    pub fn from_wire_bytes(bytes: &[u8]) -> Result<Self, crate::error::IstmoError> {
        let (envelope, _) = crate::codec::decode::<Self>(bytes)?;
        if envelope.version != PROTOCOL_VERSION {
            return Err(crate::error::IstmoError::ProtocolVersionMismatch {
                expected: PROTOCOL_VERSION,
                got: envelope.version,
            });
        }
        Ok(envelope)
    }
}

/// A single message crossing the wire.
///
/// Payload bytes are always bincode-encoded plugin-specific data; the
/// runtime never inspects them. Every variant is a distinct point in
/// the call / event / lifecycle lifecycle of a plugin surface.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum Frame {
    /// Request half of a unary or stream-returning plugin method.
    Call {
        /// Unique id assigned by the caller runtime.
        call_id: CallId,
        /// Plugin the call is addressed to.
        plugin_id: String,
        /// Instance receiving the call, or `None` for stateless plugins.
        instance_id: Option<InstanceId>,
        /// Method name as declared on the plugin trait.
        method: String,
        /// Bincode-encoded arguments tuple.
        payload: Vec<u8>,
    },

    /// Response half of a [`Frame::Call`].
    Respond {
        /// Echoes the originating [`CallId`].
        call_id: CallId,
        /// `Ok(bytes)` carries a bincode-encoded return value.
        /// `Err(bytes)` carries a bincode-encoded domain error.
        result: Result<Vec<u8>, Vec<u8>>,
    },

    /// Ask the host to cooperatively cancel an in-flight call.
    Cancel {
        /// Call to cancel.
        call_id: CallId,
    },

    /// One item on a live stream.
    Event {
        /// Stream this event belongs to.
        stream_id: StreamId,
        /// Bincode-encoded item.
        payload: Vec<u8>,
    },

    /// Terminate a stream.
    StreamEnd {
        /// Stream being closed.
        stream_id: StreamId,
        /// Why the stream ended.
        reason: StreamEndReason,
    },

    /// Construct a stateful plugin instance.
    CreateInstance {
        /// Call id used to receive the resulting [`InstanceId`].
        call_id: CallId,
        /// Plugin whose constructor is being invoked.
        plugin_id: String,
        /// Bincode-encoded constructor arguments.
        payload: Vec<u8>,
    },

    /// Release a previously created plugin instance.
    DestroyInstance {
        /// Instance to release.
        instance_id: InstanceId,
    },

    /// Fire-and-forget publish on an early-event channel.
    ///
    /// Buffered by the runtime until a subscriber attaches; the retention
    /// policy is controlled by [`EarlyEventKind`].
    EarlyEvent {
        /// Channel name (typically the plugin id or a subchannel path).
        channel: String,
        /// Retention policy for the value.
        kind: EarlyEventKind,
        /// Bincode-encoded value.
        payload: Vec<u8>,
    },

    /// Ask the peer to drop its ownership of a native handle.
    ReleaseNativeHandle {
        /// Handle id to release.
        handle_id: NativeHandleId,
    },

    /// Fire-and-forget one-way call. No [`Frame::Respond`] is expected.
    Notify {
        /// Target plugin.
        plugin_id: String,
        /// Target instance, if any.
        instance_id: Option<InstanceId>,
        /// Method name.
        method: String,
        /// Bincode-encoded arguments tuple.
        payload: Vec<u8>,
    },
}
