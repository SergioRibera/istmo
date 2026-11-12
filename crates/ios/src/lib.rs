//! Apple-platform FFI transport for the
//! [`istmo`](https://docs.rs/istmo) runtime.
//!
//! Exposes the `extern "C"` symbols the companion Swift package binds
//! to and drives the outbound envelope pump. Consumer apps typically
//! import the top-level facade instead — this crate is re-exported as
//! `istmo::ios` on iOS, tvOS, watchOS and visionOS.
//!
//! The bridging model is intentionally narrow: Swift passes a
//! [`IstmoIosCallbacks`] table of function pointers plus an opaque
//! `ctx`; the runtime hands typed frames back through that table. No
//! Rust type crosses the FFI boundary other than opaque byte slices.
//! The matching C declarations ship with the Swift package as the
//! `IstmoFFI` module (`runtime/ios/Sources/IstmoFFI/include/istmo_ffi.h`).

#![doc(html_root_url = "https://docs.rs/istmo-ios")]

pub mod assets;
pub mod entrypoint;
pub mod error;
pub mod path;

mod cdecl_exports;
mod pump;
mod state;

pub use cdecl_exports::{
    IstmoIosCallbacks, OnCallFn, OnCancelFn, OnCreateInstanceFn, OnDestroyInstanceFn, OnEventFn,
    OnNotifyFn, OnReleaseNativeHandleFn, OnRespondFn, OnStreamEndFn, istmo_ios_shutdown,
    istmo_ios_start, istmo_ios_submit_call, istmo_ios_submit_early_latest,
    istmo_ios_submit_early_queue, istmo_ios_submit_event, istmo_ios_submit_notify,
    istmo_ios_submit_response, istmo_ios_submit_stream_end,
};
pub use error::IosRuntimeError;
