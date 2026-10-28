//! Minimal desktop stub so apps can compile against `istmo-camera`
//! on Linux / macOS / Windows without pulling in a real backend.
//!
//! Every method returns [`CameraError::Backend`] with a short
//! "unsupported" message. Swap this for a real backend (e.g.
//! `nokhwa`) in a downstream crate; the plugin surface is identical.

#![cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]

use crate::{
    AccessStatus, Camera, CameraControls, CameraDevice, CameraError, CameraFrame, CameraSession,
    ConcurrentSupport, NativeHandleId, SessionConfig, StillCaptureRequest, StillPhoto,
    AppliedControls,
};

/// Zero-cost placeholder host. Returns [`CameraError::Backend`] from
/// every call.
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopCamera;

const UNSUPPORTED: &str = "camera backend unsupported on this desktop target";

#[allow(clippy::missing_errors_doc)]
impl Camera for DesktopCamera {
    async fn devices(&self) -> Result<Vec<CameraDevice>, CameraError> {
        Err(CameraError::Backend(UNSUPPORTED.into()))
    }

    async fn concurrent_support(&self) -> Result<ConcurrentSupport, CameraError> {
        Err(CameraError::Backend(UNSUPPORTED.into()))
    }

    async fn access_status(&self) -> Result<AccessStatus, CameraError> {
        Ok(AccessStatus::Denied)
    }

    async fn request_access(&self) -> Result<AccessStatus, CameraError> {
        Ok(AccessStatus::Denied)
    }

    async fn open(&self, _config: SessionConfig) -> Result<CameraSession, CameraError> {
        Err(CameraError::Backend(UNSUPPORTED.into()))
    }

    async fn close(&self, _session: NativeHandleId) -> Result<(), CameraError> {
        Err(CameraError::SessionClosed)
    }

    fn frames(&self, _session: NativeHandleId) -> flume::Receiver<CameraFrame> {
        let (_tx, rx) = flume::bounded(0);
        rx
    }

    async fn apply_controls(
        &self,
        _session: NativeHandleId,
        _controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        Err(CameraError::Backend(UNSUPPORTED.into()))
    }

    async fn capture_still(
        &self,
        _session: NativeHandleId,
        _request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError> {
        Err(CameraError::Backend(UNSUPPORTED.into()))
    }
}
