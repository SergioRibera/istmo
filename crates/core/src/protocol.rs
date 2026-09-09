//! Wire protocol: frame types, envelope and identifier newtypes.
//!
//! The message model is a small fixed set of frames. Whether a specific
//! logical operation is a request/response or a stream is a plugin-side
//! contract, not a frame distinction: streams simply omit `Respond` and emit
//! zero-or-more `Event` frames terminated by `StreamEnd`.

use core::fmt;

use bincode::{Decode, Encode};

/// Current wire version of the envelope. Bumped on any breaking frame change.
///
/// Version 2 added [`Frame::EarlyEvent`] so early-event publication crosses
/// the same `dispatch_inbound` path as every other inbound frame. Bincode 2
/// encodes enum discriminants as varint indexes in declaration order —
/// adding a new terminal variant is a wire-breaking change for readers
/// that don't know the discriminant.
///
/// Version 3 added [`Frame::ReleaseNativeHandle`] so the Rust side can tell
/// the native side that a [`crate::native_handle::NativeHandle`] is no longer
/// referenced and its backing object can be freed. Outbound-only, matching
/// the Rust-owned-lifetime model.
///
/// Version 4 added [`Frame::Notify`] — fire-and-forget one-way call. No
/// `call_id`, no [`Frame::Respond`] expected. Used for releasing
/// service-side resources (wakelocks, foreground-notification handles)
/// from `Drop` paths that must not block on a reply. Native side treats
/// unknown methods as no-ops for symmetry with `ReleaseNativeHandle`.
pub const PROTOCOL_VERSION: u16 = 4;

macro_rules! id_newtype {
    ($(#[$attr:meta])* $name:ident, $short:literal) => {
        $(#[$attr])*
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Encode, Decode)]
        #[repr(transparent)]
        pub struct $name(pub u64);

        impl $name {
            #[must_use]
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

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
    /// Identifies a single request/response exchange, or the initiator of a stream.
    CallId,
    "call"
);
id_newtype!(
    /// Identifies an ongoing stream. Shares its numeric value with the [`CallId`]
    /// that opened it, but is a distinct type to prevent misrouting between the
    /// two routing tables.
    StreamId,
    "stream"
);
id_newtype!(
    /// Identifies a plugin instance created via `CreateInstance`.
    InstanceId,
    "instance"
);
id_newtype!(
    /// Wire id of a native-side object referenced from Rust through a
    /// [`crate::native_handle::NativeHandle`].
    ///
    /// The native side owns the underlying object; Rust holds only the id
    /// plus a compile-time phantom type parameter.
    NativeHandleId,
    "native"
);

/// Reason attached to a [`Frame::StreamEnd`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum StreamEndReason {
    /// The stream ended normally.
    Complete,
    /// The stream was cancelled from the Rust side.
    Cancelled,
    /// The producer terminated the stream with a domain error payload.
    Error(Vec<u8>),
}

/// Retention shape requested for an early-event publication.
///
/// Mirrors the two [`crate::early_events`] primitives:
///
/// * [`Self::Latest`] targets `LatestValueSlot` — value semantics; late
///   subscribers observe the last-published bytes.
/// * [`Self::Queue { capacity }`] targets `PreMainQueue` — bounded FIFO
///   buffered until a subscriber attaches. Capacity is honoured only on
///   the first publication for a given channel (matches
///   `EarlyEventStore::queue`).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum EarlyEventKind {
    Latest,
    Queue { capacity: u32 },
}

/// Versioned envelope wrapping a single [`Frame`].
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct Envelope {
    pub version: u16,
    pub frame: Frame,
}

impl Envelope {
    /// Wraps a frame with the current [`PROTOCOL_VERSION`].
    #[must_use]
    pub const fn new(frame: Frame) -> Self {
        Self {
            version: PROTOCOL_VERSION,
            frame,
        }
    }

    /// Bincode-encode this envelope into the exact byte shape the wire
    /// pump ships. Convenience wrapper over
    /// [`crate::codec::encode`] so cross-process bridges do not need
    /// to reach into the codec module directly.
    pub fn to_wire_bytes(&self) -> Result<Vec<u8>, crate::error::CodecError> {
        crate::codec::encode(self)
    }

    /// Reverse of [`Self::to_wire_bytes`]: decode a bincoded envelope
    /// and verify its version matches [`PROTOCOL_VERSION`].
    ///
    /// # Errors
    /// Returns [`crate::error::IstmoError::ProtocolVersionMismatch`] when
    /// the decoded envelope declares a different version; codec errors
    /// bubble up untouched.
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

/// The complete set of wire frames exchanged between Rust and the native side.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub enum Frame {
    /// Invoke a plugin method. Depending on the plugin contract this may be
    /// followed by a single `Respond` or a series of `Event` frames ending in
    /// a `StreamEnd`.
    Call {
        call_id: CallId,
        plugin_id: String,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    },
    /// Reply to a `Call`, either with an encoded result or an encoded error.
    Respond {
        call_id: CallId,
        result: Result<Vec<u8>, Vec<u8>>,
    },
    /// Cancel an in-flight call or stream identified by `call_id`.
    Cancel { call_id: CallId },
    /// One element emitted on an open stream.
    Event {
        stream_id: StreamId,
        payload: Vec<u8>,
    },
    /// Marks the end of a stream. No further `Event`s for `stream_id` will arrive.
    StreamEnd {
        stream_id: StreamId,
        reason: StreamEndReason,
    },
    /// Ask the native factory to create a new plugin instance.
    CreateInstance {
        call_id: CallId,
        plugin_id: String,
        payload: Vec<u8>,
    },
    /// Tear down a previously created instance. Fire-and-forget.
    DestroyInstance { instance_id: InstanceId },
    /// Publish a payload to an early-event channel. Fire-and-forget from
    /// the native side; the runtime routes it into the
    /// [`crate::early_events::EarlyEventStore`] instead of into the
    /// per-call routing tables.
    EarlyEvent {
        channel: String,
        kind: EarlyEventKind,
        payload: Vec<u8>,
    },
    /// Tell the native side to release the object backing
    /// [`NativeHandleId`]. Outbound-only, fire-and-forget: the native side
    /// must treat unknown ids as no-ops so a late release racing a shutdown
    /// is harmless.
    ReleaseNativeHandle { handle_id: NativeHandleId },
    /// Fire-and-forget invocation. No `call_id`, no `Respond` expected —
    /// the receiver dispatches like a Call and discards the outcome.
    /// Used for `Drop`-time release paths and other one-way signals
    /// where blocking on a reply is not acceptable.
    Notify {
        plugin_id: String,
        instance_id: Option<InstanceId>,
        method: String,
        payload: Vec<u8>,
    },
}
