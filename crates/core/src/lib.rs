//! Wire protocol, per-process runtime and routing primitives for
//! [`istmo`](https://docs.rs/istmo).
//!
//! This crate is transport-agnostic: it defines the frame format, the
//! [`Runtime`] that dispatches inbound frames to registered plugins,
//! and the routing tables that pair calls with their responses. Actual
//! I/O (JNI, Swift FFI, in-process shortcuts) lives in sibling crates
//! such as [`istmo-android`](https://docs.rs/istmo-android) and
//! [`istmo-ios`](https://docs.rs/istmo-ios).
//!
//! Most users pull the whole framework in via the top-level
//! [`istmo`](https://docs.rs/istmo) facade and never depend on
//! `istmo-core` directly.
//!
//! # Modules
//!
//! - [`protocol`] — [`Frame`], [`Envelope`], typed IDs and
//!   [`PROTOCOL_VERSION`].
//! - [`runtime`] — the per-process [`Runtime`] plus its wiring types
//!   ([`RuntimeInit`], [`RuntimeConfig`]).
//! - [`dispatch`] — the [`Plugin`] and [`Dispatch`] contracts, the
//!   [`Outcome`] returned by dispatchers, and [`CancelToken`].
//! - [`routing`] — pending-call / stream / instance registries.
//! - [`message`] — the [`Message`] trait every wire type implements.
//! - [`native_handle`] — opaque platform-owned references
//!   ([`NativeHandle`], [`NativeHandleId`]).
//! - [`early_events`] — buffers for values published before their first
//!   subscriber attaches.
//! - [`main_thread`] — abstraction over "run this on the platform's
//!   main thread" (Android `Handler`, iOS main queue, or an inline
//!   executor on desktop).
//! - [`typed_stream`] — typed wrapper around a raw byte-stream receiver.
//! - [`codec`] — bincode configuration used across the wire.
//! - [`error`] — [`IstmoError`] and [`CodecError`].

#![doc(html_root_url = "https://docs.rs/istmo-core")]

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
pub use crate::typed_stream::{StreamItem, TypedStream};

#[doc(hidden)]
pub mod __private {
    pub use pollster::block_on;
    pub use tracing;
}
