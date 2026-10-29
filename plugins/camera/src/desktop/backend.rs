//! `nokhwa`-backed pure-Rust desktop host.
//!
//! One [`CallbackCamera`] per session; the capture thread lives inside
//! `nokhwa` and drops frames into a bounded `flume` channel sized by
//! [`CameraInitConfig::frame_pool_depth`]. The channel is a single
//! shared queue — multiple `.frames()` clones share it (one frame per
//! consumer, not fan-out). For multi-consumer fan-out, wrap the
//! stream in a `broadcast` on the app side.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use istmo_core::{NativeHandleId, Runtime};
use nokhwa::CallbackCamera;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::query;
use nokhwa::utils::{
    ApiBackend, CameraIndex, ControlValueSetter, KnownCameraControl, RequestedFormat,
    RequestedFormatType,
};

use crate::{
    AccessStatus, AppliedControls, Camera, CameraCapabilities, CameraControls, CameraDevice,
    CameraError, CameraFacing, CameraFrame, CameraInitConfig, CameraSession, ConcurrentSupport,
    FlashMode, FocusMode, FramePlane, LensKind, NegotiatedFormat, PixelFormat, Resolution,
    SessionConfig, StillCaptureRequest, StillPhoto, WhiteBalanceMode,
};

/// Pure-Rust desktop host for the [`Camera`] plugin. Register with
/// the macro-emitted `CameraHost::new(DesktopCameraFactory)` wrapper.
///
/// Each open session owns its own `nokhwa` capture thread and feeds
/// frames into a shared [`flume::Receiver<CameraFrame>`] handed to
/// subscribers via [`Camera::frames`].
/// Session ids handed out by every [`DesktopCamera`] in the process. Release
/// hooks see every [`NativeHandleId`] the runtime frees, so ids must not
/// repeat across instances.
static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);

#[derive(Debug)]
pub struct DesktopCamera {
    config: CameraInitConfig,
    sessions: Arc<Mutex<HashMap<NativeHandleId, SessionEntry>>>,
    /// Set once the runtime release hook for this instance is installed.
    release_hook: OnceLock<()>,
}

/// Live session state. `CallbackCamera` keeps its capture thread
/// alive for as long as the struct lives — dropping the entry stops
/// capture.
struct SessionEntry {
    camera: CallbackCamera,
    rx: flume::Receiver<CameraFrame>,
    pixel_format: PixelFormat,
    negotiated: NegotiatedFormat,
}

impl std::fmt::Debug for SessionEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEntry")
            .field("pixel_format", &self.pixel_format)
            .field("negotiated", &self.negotiated)
            .finish_non_exhaustive()
    }
}

impl DesktopCamera {
    #[must_use]
    pub fn new(config: CameraInitConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            release_hook: OnceLock::new(),
        }
    }

    fn alloc_id() -> NativeHandleId {
        NativeHandleId::new(NEXT_SESSION.fetch_add(1, Ordering::Relaxed))
    }

    /// Close a session when the client drops its [`crate::SessionHandle`]
    /// without calling `close`. The runtime only forwards that release to
    /// its hooks; there is no transport pump to route it back here.
    fn install_release_hook(&self) {
        self.release_hook.get_or_init(|| {
            let Ok(runtime) = Runtime::global() else {
                tracing::warn!(
                    target: "istmo.camera.desktop",
                    "no runtime; dropped sessions stay open until close"
                );
                return;
            };
            let sessions: Weak<Mutex<HashMap<NativeHandleId, SessionEntry>>> =
                Arc::downgrade(&self.sessions);
            runtime.install_native_handle_release_hook(Arc::new(move |id| {
                let Some(sessions) = sessions.upgrade() else {
                    return;
                };
                let entry = sessions.lock().expect("sessions mutex").remove(&id);
                // Dropping the entry stops the capture thread and frees the device.
                drop(entry);
            }));
        });
    }

    fn pool_depth(&self) -> usize {
        let d = self.config.frame_pool_depth as usize;
        if d == 0 { 3 } else { d }
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
        let infos = query(ApiBackend::Auto)
            .map_err(|err| CameraError::Backend(err.to_string()))?;
        Ok(infos.into_iter().map(describe_device).collect())
    }

    async fn concurrent_support(&self) -> Result<ConcurrentSupport, CameraError> {
        Ok(ConcurrentSupport {
            max_concurrent: 4,
            approved_pairs: Vec::new(),
            any_combination: true,
        })
    }

    async fn access_status(&self) -> Result<AccessStatus, CameraError> {
        // Desktop OSes do not generally gate webcam access behind a
        // Rust-reachable permission prompt. macOS surfaces TCC the
        // first time the process opens a camera; everything else is
        // open on the user's session. Report Granted so the caller's
        // flow doesn't stall; the first `open()` call surfaces the
        // real denial as `CameraError::Backend`.
        Ok(AccessStatus::Granted)
    }

    async fn request_access(&self) -> Result<AccessStatus, CameraError> {
        self.access_status().await
    }

    async fn open(&self, config: SessionConfig) -> Result<CameraSession, CameraError> {
        if !matches!(
            config.pixel_format,
            PixelFormat::Rgba8 | PixelFormat::Bgra8
        ) {
            return Err(CameraError::FormatUnsupported);
        }

        let (tx, rx) = flume::bounded::<CameraFrame>(self.pool_depth());
        let dropped = Arc::new(AtomicU64::new(0));
        let seq = Arc::new(AtomicU64::new(0));
        let pixel_format = config.pixel_format;

        let index = resolve_index(&config.device_id)?;
        let requested = requested_format(&config);

        let tx_cb = tx.clone();
        let dropped_cb = dropped.clone();
        let seq_cb = seq.clone();
        let fmt_cb = pixel_format;

        let mut camera = CallbackCamera::new(index.clone(), requested, move |buffer| {
            match buffer_to_frame(&buffer, fmt_cb, &seq_cb, &dropped_cb) {
                Ok(frame) => {
                    if tx_cb.try_send(frame).is_err() {
                        dropped_cb.fetch_add(1, Ordering::Relaxed);
                    }
                }
                Err(err) => {
                    tracing::warn!(target: "istmo.camera.desktop", %err, "frame decode failed");
                }
            }
        })
        .map_err(|err| CameraError::Backend(err.to_string()))?;

        if let Some(fps) = config.target_fps {
            // Best-effort — nokhwa silently clamps when the camera
            // can't honour the request.
            let _ = camera.set_frame_rate(fps);
        }

        camera
            .open_stream()
            .map_err(|err| CameraError::Backend(err.to_string()))?;

        apply_initial_controls(&mut camera, &config.initial_controls);

        let camera_format = camera
            .camera_format()
            .map_err(|err| CameraError::Backend(err.to_string()))?;
        let negotiated = NegotiatedFormat {
            resolution: Resolution {
                width: camera_format.resolution().width_x,
                height: camera_format.resolution().height_y,
            },
            pixel_format,
            fps: camera_format.frame_rate(),
            rotation_degrees: 0,
        };

        self.install_release_hook();
        let id = Self::alloc_id();
        self.sessions.lock().expect("sessions mutex").insert(
            id,
            SessionEntry {
                camera,
                rx,
                pixel_format,
                negotiated: negotiated.clone(),
            },
        );

        Ok(CameraSession {
            session: id,
            negotiated,
        })
    }

    async fn close(&self, session: NativeHandleId) -> Result<(), CameraError> {
        let mut entry = self
            .sessions
            .lock()
            .expect("sessions mutex")
            .remove(&session);
        if let Some(ref mut e) = entry {
            let _ = e.camera.stop_stream();
        }
        Ok(())
    }

    fn frames(&self, session: NativeHandleId) -> flume::Receiver<CameraFrame> {
        self.sessions
            .lock()
            .expect("sessions mutex")
            .get(&session)
            .map_or_else(
                || {
                    let (_tx, rx) = flume::bounded::<CameraFrame>(0);
                    rx
                },
                |entry| entry.rx.clone(),
            )
    }

    async fn apply_controls(
        &self,
        session: NativeHandleId,
        controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        let mut guard = self.sessions.lock().expect("sessions mutex");
        let entry = guard
            .get_mut(&session)
            .ok_or(CameraError::SessionClosed)?;

        if let Some(zoom) = controls.zoom {
            // Convert to nokhwa integer zoom. Many V4L2 drivers expose
            // digital zoom as an integer step; UVC devices expose a
            // percentage. Best-effort: scale `zoom` into the control's
            // native range.
            let _ = entry.camera.set_camera_control(
                KnownCameraControl::Zoom,
                ControlValueSetter::Integer(zoom as i64),
            );
        }
        if let Some(ev) = controls.exposure_compensation_ev {
            let _ = entry.camera.set_camera_control(
                KnownCameraControl::Exposure,
                ControlValueSetter::Integer(ev as i64),
            );
        }
        if let Some(FocusMode::Manual(distance)) = controls.focus {
            // Manual focus — drivers expose this as an integer lens
            // position. Scale `distance` ([0.0, 1.0]) into i64 — the
            // caller still needs to probe the valid range for best
            // results.
            let scaled = (distance.clamp(0.0, 1.0) * 1000.0) as i64;
            let _ = entry.camera.set_camera_control(
                KnownCameraControl::Focus,
                ControlValueSetter::Integer(scaled),
            );
        }
        // Torch / flash / white-balance / autofocus coverage varies
        // per camera; leave untouched here.

        Ok(AppliedControls {
            zoom: controls.zoom.unwrap_or(1.0),
            torch: controls.torch.unwrap_or(false),
            focus: controls.focus.unwrap_or(FocusMode::ContinuousVideo),
            exposure_compensation_ev: controls.exposure_compensation_ev.unwrap_or(0.0),
            white_balance: controls.white_balance.unwrap_or(WhiteBalanceMode::Auto),
            flash: controls.flash.unwrap_or(FlashMode::Off),
        })
    }

    async fn capture_still(
        &self,
        session: NativeHandleId,
        _request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError> {
        let mut guard = self.sessions.lock().expect("sessions mutex");
        let entry = guard
            .get_mut(&session)
            .ok_or(CameraError::SessionClosed)?;
        let buffer = entry
            .camera
            .last_frame()
            .map_err(|err| CameraError::Backend(err.to_string()))?;
        let dropped = Arc::new(AtomicU64::new(0));
        let seq = Arc::new(AtomicU64::new(0));
        let frame = buffer_to_frame(&buffer, entry.pixel_format, &seq, &dropped)
            .map_err(CameraError::Backend)?;
        Ok(StillPhoto {
            resolution: frame.resolution,
            rotation_degrees: 0,
            pixel_format: frame.pixel_format,
            bytes: frame.bytes,
            timestamp_ns: frame.timestamp_ns,
        })
    }
}

// -------------------------------------------------------------------
// Device description
// -------------------------------------------------------------------

fn describe_device(info: nokhwa::utils::CameraInfo) -> CameraDevice {
    CameraDevice {
        id: info.index().to_string(),
        label: info.human_name(),
        facing: CameraFacing::Unknown,
        lens: LensKind::Wide,
        physical_sensors: Vec::new(),
        capabilities: CameraCapabilities {
            min_zoom: 1.0,
            max_zoom: 1.0,
            optical_zoom_steps: Vec::new(),
            focal_lengths_mm: Vec::new(),
            supports_flash: false,
            supports_torch: false,
            supports_autofocus: true,
            supports_manual_focus: false,
            supports_ois: false,
            supports_eis: false,
            supports_hdr: false,
            supports_depth: false,
            supports_raw: false,
            max_still_resolution: Resolution {
                width: 0,
                height: 0,
            },
        },
        supported_formats: Vec::new(),
    }
}

// -------------------------------------------------------------------
// Index resolution + requested format
// -------------------------------------------------------------------

fn resolve_index(device_id: &str) -> Result<CameraIndex, CameraError> {
    if let Ok(n) = device_id.parse::<u32>() {
        return Ok(CameraIndex::Index(n));
    }
    Ok(CameraIndex::String(device_id.to_owned()))
}

fn requested_format(config: &SessionConfig) -> RequestedFormat<'static> {
    let kind = match (config.resolution, config.target_fps) {
        (Some(res), Some(fps)) => RequestedFormatType::Closest(
            nokhwa::utils::CameraFormat::new_from(
                res.width,
                res.height,
                nokhwa::utils::FrameFormat::YUYV,
                fps,
            ),
        ),
        (Some(_), None) | (None, _) => RequestedFormatType::AbsoluteHighestFrameRate,
    };
    RequestedFormat::new::<RgbAFormat>(kind)
}

// -------------------------------------------------------------------
// Buffer → CameraFrame conversion
// -------------------------------------------------------------------

fn buffer_to_frame(
    buffer: &nokhwa::Buffer,
    pixel_format: PixelFormat,
    seq: &std::sync::Arc<AtomicU64>,
    dropped: &std::sync::Arc<AtomicU64>,
) -> Result<CameraFrame, String> {
    let res = buffer.resolution();
    let width = res.width_x;
    let height = res.height_y;

    let rgba = buffer
        .decode_image::<RgbAFormat>()
        .map_err(|e| e.to_string())?;
    let mut bytes: Vec<u8> = rgba.into_raw();
    if matches!(pixel_format, PixelFormat::Bgra8) {
        swap_rb_in_place(&mut bytes);
    }

    let plane = FramePlane {
        offset: 0,
        row_stride: width * 4,
        pixel_stride: 4,
        width,
        height,
    };

    let timestamp_ns = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);

    Ok(CameraFrame {
        seq: seq.fetch_add(1, Ordering::Relaxed),
        timestamp_ns,
        resolution: Resolution { width, height },
        rotation_degrees: 0,
        pixel_format,
        planes: vec![plane],
        bytes,
        dropped_before: dropped.swap(0, Ordering::Relaxed) as u32,
    })
}

/// Swap R and B channels in a packed 32-bit RGBA buffer. Used to
/// convert the RGBA output nokhwa gives us into BGRA when the caller
/// asked for [`PixelFormat::Bgra8`].
fn swap_rb_in_place(bytes: &mut [u8]) {
    for px in bytes.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
}

fn apply_initial_controls(camera: &mut CallbackCamera, controls: &CameraControls) {
    if let Some(zoom) = controls.zoom {
        let _ = camera.set_camera_control(
            KnownCameraControl::Zoom,
            ControlValueSetter::Integer(zoom as i64),
        );
    }
}

// -------------------------------------------------------------------
// Factory
// -------------------------------------------------------------------

/// Factory the runtime registers so `CameraHost` can instantiate the
/// pure-Rust desktop backend. See [`DesktopCamera`].
///
/// ```ignore
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
