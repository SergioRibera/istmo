//! macOS backends: ScreenCaptureKit (`macos-sck`) for video and audio.
//!
//! The Rust desktop host owns macOS: `ffi` wraps the framework calls,
//! `sck` is the video backend, `session` turns sample buffers into
//! frames. Without the feature the host
//! answers `NoBackendCompiled`.

#[cfg(feature = "macos-sck")]
mod ffi;
#[cfg(feature = "macos-sck")]
mod sck;
#[cfg(feature = "macos-sck")]
mod session;

use std::sync::Arc;

use crate::types::BackendId;

use super::backend::{AudioBackend, Backend};

#[cfg(feature = "macos-sck")]
pub use self::ffi::IoSurfaceFrame;

/// Backends the plugin knows on macOS, compiled or not; also the `Auto`
/// order.
pub(crate) const KNOWN: [BackendId; 1] = [BackendId::MacosSck];

/// Every compiled video backend.
pub(crate) fn backends() -> Vec<Arc<dyn Backend>> {
    #[cfg(feature = "macos-sck")]
    {
        vec![sck::backend()]
    }
    #[cfg(not(feature = "macos-sck"))]
    {
        Vec::new()
    }
}

/// The compiled audio backend: none yet.
pub(crate) const fn audio() -> Option<Arc<dyn AudioBackend>> {
    None
}

/// The host time clock now, in nanoseconds (what sample buffers are
/// stamped on).
#[cfg(feature = "macos-sck")]
pub(crate) fn host_time_ns() -> Option<u64> {
    ffi::host_time_ns()
}
