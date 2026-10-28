//! Desktop host for the camera plugin.
//!
//! Pure-Rust capture backed by [`nokhwa`]:
//! - Linux → V4L2
//! - macOS → AVFoundation (through `nokhwa-bindings-macos`)
//! - Windows → Media Foundation
//!
//! Gated behind the `desktop` feature so mobile builds don't drag the
//! native-capture dep. On a non-desktop target the module compiles to
//! an empty surface — the Rust-hosted path is for apps that run
//! outside Android / iOS.
//!
//! The host owns one Tokio-free dispatch map keyed by
//! [`NativeHandleId`]; each session carries a background capture
//! thread (`nokhwa::CallbackCamera`) feeding frames through a
//! bounded `flume` channel. Backpressure matches the mobile backend:
//! the oldest frame is dropped when the consumer falls behind and
//! the delivered `CameraFrame` carries a non-zero `dropped_before`.
//!
//! ```ignore
//! use std::sync::Arc;
//! use istmo_camera::{DesktopCamera, CameraInitConfig};
//! let host = Arc::new(DesktopCamera::new(CameraInitConfig::default()));
//! // Register with the runtime via the host-side helper (see docs).
//! ```

#![cfg(all(
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    feature = "desktop"
))]

mod backend;

pub use backend::{DesktopCamera, DesktopCameraFactory};
