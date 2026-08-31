//! Wire protocol: frame types, envelope and identifier newtypes.
//!
//! The message model is a small fixed set of frames. Whether a specific
//! logical operation is a request/response or a stream is a plugin-side
//! contract, not a frame distinction: streams simply omit `Respond` and emit
//! zero-or-more `Event` frames terminated by `StreamEnd`.

use core::fmt;

use bincode::{Decode, Encode};

/// Current wire version of the envelope. Bumped on any breaking frame change.
pub const PROTOCOL_VERSION: u16 = 1;

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
}
