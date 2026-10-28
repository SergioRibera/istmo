//! Pure-Rust desktop camera host.
//!
//! Scaffolding stage: declares the `DesktopCamera` + `DesktopCameraFactory`
//! surface and threads it through the plugin trait. Every method
//! returns `CameraError::Backend("unimplemented")` so the compile path
//! is clean while the `nokhwa` pipeline lands in a follow-up commit.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use istmo_core::NativeHandleId;

use crate::{
    AccessStatus, AppliedControls, Camera, CameraControls, CameraDevice, CameraError, CameraFrame,
    CameraInitConfig, CameraSession, ConcurrentSupport, SessionConfig, StillCaptureRequest,
    StillPhoto,
};

/// Pure-Rust desktop host for the [`Camera`] plugin. Register with
/// the macro-emitted `CameraHost::new(CameraFactoryImpl)` wrapper.
///
/// The host owns one `nokhwa`-backed capture thread per open session;
/// each session is addressed by a monotonic [`NativeHandleId`] stored
/// alongside its outbound frame channel.
#[derive(Debug)]
pub struct DesktopCamera {
    #[allow(dead_code)] // consumed once the real pipeline lands.
    config: CameraInitConfig,
    sessions: Mutex<HashMap<NativeHandleId, SessionEntry>>,
    next_session: AtomicU64,
}

#[derive(Debug)]
struct SessionEntry {
    rx: flume::Receiver<CameraFrame>,
}

impl DesktopCamera {
    #[must_use]
    pub fn new(config: CameraInitConfig) -> Self {
        Self {
            config,
            sessions: Mutex::new(HashMap::new()),
            next_session: AtomicU64::new(1),
        }
    }

    #[allow(dead_code)] // consumed once the real pipeline lands.
    fn alloc_id(&self) -> NativeHandleId {
        NativeHandleId::new(self.next_session.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for DesktopCamera {
    fn default() -> Self {
        Self::new(CameraInitConfig::default())
    }
}

#[allow(clippy::missing_errors_doc)]
impl Camera for DesktopCamera {
    async fn devices(&self) -> Result<Vec<CameraDevice>, CameraError> {
        Err(CameraError::Backend("desktop backend scaffolded, pipeline pending".into()))
    }

    async fn concurrent_support(&self) -> Result<ConcurrentSupport, CameraError> {
        Ok(ConcurrentSupport {
            max_concurrent: 4,
            approved_pairs: Vec::new(),
            any_combination: true,
        })
    }

    async fn access_status(&self) -> Result<AccessStatus, CameraError> {
        Ok(AccessStatus::Granted)
    }

    async fn request_access(&self) -> Result<AccessStatus, CameraError> {
        Ok(AccessStatus::Granted)
    }

    async fn open(&self, _config: SessionConfig) -> Result<CameraSession, CameraError> {
        Err(CameraError::Backend("desktop backend scaffolded, pipeline pending".into()))
    }

    async fn close(&self, session: NativeHandleId) -> Result<(), CameraError> {
        self.sessions
            .lock()
            .expect("sessions mutex")
            .remove(&session);
        Ok(())
    }

    fn frames(&self, session: NativeHandleId) -> flume::Receiver<CameraFrame> {
        self.sessions
            .lock()
            .expect("sessions mutex")
            .get(&session)
            .map_or_else(
                || {
                    let (_tx, rx) = flume::bounded(0);
                    rx
                },
                |entry| entry.rx.clone(),
            )
    }

    async fn apply_controls(
        &self,
        _session: NativeHandleId,
        _controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        Err(CameraError::Backend("desktop backend scaffolded, pipeline pending".into()))
    }

    async fn capture_still(
        &self,
        _session: NativeHandleId,
        _request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError> {
        Err(CameraError::Backend("desktop backend scaffolded, pipeline pending".into()))
    }
}

/// Factory the runtime registers so `CameraHost` can instantiate the
/// pure-Rust desktop backend. See [`DesktopCamera`].
///
/// ```ignore
/// use std::sync::Arc;
/// use istmo_camera::{CameraHost, DesktopCameraFactory};
///
/// runtime.register_host(CameraHost::new(DesktopCameraFactory));
/// ```
#[derive(Debug, Default, Clone, Copy)]
pub struct DesktopCameraFactory;

impl crate::CameraFactory for DesktopCameraFactory {
    type Instance = DesktopCamera;

    fn create(&self, config: CameraInitConfig) -> DesktopCamera {
        DesktopCamera::new(config)
    }
}
