//! Core transport, protocol and per-process runtime for the istmo framework.
//!
//! See the workspace `PLAN.md` for the architectural context. This crate
//! implements milestone **M0**: the wire message types, the codec, the
//! routing tables and the per-process `Runtime`. All I/O is mocked; no
//! JNI/FFI code lives here yet.

mod sync;

pub mod codec;
pub mod dispatch;
pub mod early_events;
pub mod error;
pub mod main_thread;
pub mod message;
pub mod native_handle;
pub mod protocol;
pub mod routing;
pub mod runtime;
pub mod typed_stream;

pub use crate::dispatch::{CancelToken, Dispatch, DispatchError, DispatchFuture, Outcome, Plugin};
pub use crate::error::{CodecError, IstmoError};
pub use crate::main_thread::{InlineMainThread, MainThread, MockMainThread, Task};
pub use crate::message::Message;
pub use crate::native_handle::NativeHandle;
pub use crate::protocol::{
    CallId, EarlyEventKind, Envelope, Frame, InstanceId, NativeHandleId, PROTOCOL_VERSION,
    StreamEndReason, StreamId,
};
pub use crate::routing::{CallResult, InstanceEntry, RoutingTables, StreamMessage};
pub use crate::runtime::{
    CallHandle, DEFAULT_OUTBOUND_CAPACITY, Runtime, RuntimeConfig, RuntimeInit, StreamHandle,
};

/// Implementation-detail re-exports used by macro-generated code. Nothing
/// here is part of the stable public API.
#[doc(hidden)]
pub mod __private {
    pub use pollster::block_on;
    pub use tracing;
}
pub use crate::typed_stream::{StreamItem, TypedStream};
