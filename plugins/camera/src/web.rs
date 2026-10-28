//! Browser host for the camera plugin.
//!
//! Gated behind `#[cfg(all(target_arch = "wasm32", feature = "web"))]`.
//!
//! # Status
//!
//! Scaffolding. The current host answers [`CameraError::Backend`]
//! from every method so apps compiling against `istmo-camera` on
//! `wasm32-unknown-unknown` get a clean compile with the API surface
//! wired up end-to-end. The real pipeline hasn't landed yet because
//! `nokhwa` 0.10 commented out its `js_camera` module (`feature =
//! "input-jscam"` is declared but the `pub mod js_camera;` line is
//! disabled in `lib.rs`). The replacement is a direct
//! `web-sys::MediaDevices` wrapper — the roadmap below lays out the
//! moving parts so a follow-up PR can wire them in a single pass.
//!
//! # Roadmap
//!
//! The intended implementation sits on these browser primitives:
//!
//! - `navigator.mediaDevices.enumerateDevices()` → populate
//!   [`CameraDevice`] by filtering `kind == "videoinput"`. Each
//!   `MediaDeviceInfo::deviceId()` becomes the opaque id;
//!   `label()` the human name. Capability info is thin until the
//!   first `getUserMedia` call hands back a track — before that,
//!   browsers typically redact labels for privacy. Fallback: emit a
//!   single `"default"` entry with [`CameraFacing::Unknown`].
//!
//! - `navigator.mediaDevices.getUserMedia({ video: { deviceId,
//!   width, height, frameRate } })` → [`Camera::open`] awaits a
//!   `MediaStream`, grabs the first `VideoTrack`, attaches it to an
//!   `HTMLVideoElement` (invisible), and starts a frame pump.
//!
//! - Frame pump (two viable paths):
//!     1. **MediaStreamTrackProcessor** (Chrome 94+, limited Safari)
//!        — stream the native `VideoFrame`s, use `copyTo()` to pull
//!        bytes into a Rust-owned `Vec<u8>`. Zero-copy adjacent, no
//!        canvas detour.
//!     2. **Canvas 2D + `requestAnimationFrame`** (universal) —
//!        `drawImage(video, 0, 0)` into a hidden `<canvas>`, then
//!        `getImageData` yields `Uint8ClampedArray` RGBA bytes.
//!        Cheap enough for most app logic; expensive for 4K.
//!   The pump pushes [`CameraFrame`] into a `flume` channel sized
//!   by [`CameraInitConfig::frame_pool_depth`] — mirror of the
//!   mobile / desktop backpressure model.
//!
//! - Controls — browser capability-dependent. `MediaStreamTrack
//!   ::applyConstraints({ advanced: [{ zoom, torch, exposureMode,
//!   whiteBalanceMode }] })` covers everything Chrome exposes; Safari
//!   silently ignores. Honour the trait, clamp to supported ranges,
//!   return the realised state as [`AppliedControls`].
//!
//! - Permissions — `navigator.permissions.query({ name: 'camera' })`
//!   maps directly onto [`AccessStatus`]. `request_access()` resolves
//!   by calling `getUserMedia` once and immediately stopping the
//!   returned track.
//!
//! - Session release — stash the `MediaStream`, the hidden
//!   `<video>` and `<canvas>` elements, and the rAF callback handle
//!   in `WebSessionEntry`. [`Camera::close`] calls
//!   `track.stop()` on every track + cancels the rAF loop; dropping
//!   `WebCamera` fires the same path.
//!
//! Required dependencies (gate on the `web` feature once the real
//! impl lands):
//!
//! ```toml
//! [target.'cfg(target_arch = "wasm32")'.dependencies]
//! wasm-bindgen = "0.2"
//! wasm-bindgen-futures = "0.4"
//! js-sys = "0.3"
//! web-sys = { version = "0.3", features = [
//!     "MediaDevices", "MediaStream", "MediaStreamTrack",
//!     "MediaStreamConstraints", "MediaTrackConstraints",
//!     "MediaDeviceInfo", "MediaDeviceKind", "Navigator",
//!     "Window", "HtmlVideoElement", "HtmlCanvasElement",
//!     "CanvasRenderingContext2d", "ImageData", "Document",
//!     "Permissions", "PermissionDescriptor", "PermissionState",
//! ] }
//! ```

#![cfg(all(target_arch = "wasm32", feature = "web"))]

use istmo_core::NativeHandleId;

use crate::{
    AccessStatus, AppliedControls, Camera, CameraControls, CameraDevice, CameraError, CameraFrame,
    CameraInitConfig, CameraSession, ConcurrentSupport, SessionConfig, StillCaptureRequest,
    StillPhoto,
};

const PENDING: &str = "web camera host pending — see src/web.rs roadmap";

/// Browser host for the camera plugin. Currently a scaffold —
/// methods return [`CameraError::Backend`] so wasm32 downstreams
/// compile against the surface while the real pipeline lands in a
/// follow-up commit.
#[derive(Debug)]
pub struct WebCamera {
    #[allow(dead_code)] // consumed by the real pipeline.
    config: CameraInitConfig,
}

impl WebCamera {
    #[must_use]
    pub const fn new(config: CameraInitConfig) -> Self {
        Self { config }
    }
}

impl Default for WebCamera {
    fn default() -> Self {
        Self::new(CameraInitConfig { frame_pool_depth: 3 })
    }
}

#[allow(clippy::missing_errors_doc)]
impl Camera for WebCamera {
    async fn devices(&self) -> Result<Vec<CameraDevice>, CameraError> {
        Err(CameraError::Backend(PENDING.into()))
    }

    async fn concurrent_support(&self) -> Result<ConcurrentSupport, CameraError> {
        Ok(ConcurrentSupport {
            max_concurrent: 2,
            approved_pairs: Vec::new(),
            any_combination: true,
        })
    }

    async fn access_status(&self) -> Result<AccessStatus, CameraError> {
        Ok(AccessStatus::NotDetermined)
    }

    async fn request_access(&self) -> Result<AccessStatus, CameraError> {
        Err(CameraError::Backend(PENDING.into()))
    }

    async fn open(&self, _config: SessionConfig) -> Result<CameraSession, CameraError> {
        Err(CameraError::Backend(PENDING.into()))
    }

    async fn close(&self, _session: NativeHandleId) -> Result<(), CameraError> {
        Ok(())
    }

    fn frames(&self, _session: NativeHandleId) -> flume::Receiver<CameraFrame> {
        let (_tx, rx) = flume::bounded::<CameraFrame>(0);
        rx
    }

    async fn apply_controls(
        &self,
        _session: NativeHandleId,
        _controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        Err(CameraError::Backend(PENDING.into()))
    }

    async fn capture_still(
        &self,
        _session: NativeHandleId,
        _request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError> {
        Err(CameraError::Backend(PENDING.into()))
    }
}

/// Factory the runtime registers for the wasm32 host. Mirror of
/// [`crate::DesktopCameraFactory`] — see [`WebCamera`] for status.
#[derive(Debug, Default, Clone, Copy)]
pub struct WebCameraFactory;

impl crate::CameraFactory for WebCameraFactory {
    type Instance = WebCamera;

    fn create(&self, config: CameraInitConfig) -> WebCamera {
        WebCamera::new(config)
    }
}
