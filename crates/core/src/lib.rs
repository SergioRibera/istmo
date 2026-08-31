//! Core transport, protocol and per-process runtime for the istmo framework.
//!
//! See the workspace `PLAN.md` for the architectural context. This crate
//! implements milestone **M0**: the wire message types, the codec, the
//! routing tables and the per-process `Runtime`. All I/O is mocked; no
//! JNI/FFI code lives here yet.

mod sync;

pub mod codec;
pub mod early_events;
pub mod error;
pub mod main_thread;
pub mod protocol;
pub mod routing;
pub mod runtime;

pub use crate::error::{CodecError, IstmoError};
pub use crate::main_thread::{InlineMainThread, MainThread, MockMainThread, Task};
pub use crate::protocol::{
    CallId, Envelope, Frame, InstanceId, PROTOCOL_VERSION, StreamEndReason, StreamId,
};
pub use crate::routing::{CallResult, InstanceEntry, RoutingTables, StreamMessage};
pub use crate::runtime::{
    CallHandle, DEFAULT_OUTBOUND_CAPACITY, Runtime, RuntimeConfig, RuntimeInit, StreamHandle,
};
