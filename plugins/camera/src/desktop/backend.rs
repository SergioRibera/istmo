//! `nokhwa`-backed pure-Rust desktop host.
//!
//! One [`CallbackCamera`] per session; the capture thread lives inside
//! `nokhwa` and publishes every frame to each `.frames()` subscriber,
//! each with its own queue of [`CameraInitConfig::frame_pool_depth`]
//! frames (oldest dropped when a subscriber falls behind).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};
use std::time::SystemTime;

use istmo_core::sync::lock;
use istmo_core::{NativeHandleId, Runtime};
use nokhwa::CallbackCamera;
use nokhwa::pixel_format::RgbAFormat;
use nokhwa::query;
use nokhwa::utils::{
    ApiBackend, CameraIndex, ControlValueSetter, KnownCameraControl, RequestedFormat,
    RequestedFormatType,
};

use crate::fanout::FrameFanout;
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
/// frames to every subscriber of [`Camera::frames`], each through its
/// own bounded queue.
///
/// Session ids come from [`Runtime::alloc_native_handle_id`], so they
/// never collide with ids a Kotlin / Swift backend or another Rust host
/// hands out through the same runtime.
#[derive(Debug)]
pub struct DesktopCamera {
    config: CameraInitConfig,
    sessions: Arc<Mutex<HashMap<NativeHandleId, SessionEntry>>>,
    /// Runtime the release hook was installed on; also the id allocator.
    /// `None` when no process runtime existed on first use.
    runtime: OnceLock<Option<Weak<Runtime>>>,
}

/// Live session state. `CallbackCamera` keeps its capture thread
/// alive for as long as the struct lives — dropping the entry stops
/// capture.
struct SessionEntry {
    camera: CallbackCamera,
    fanout: Arc<FrameFanout>,
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

impl Drop for SessionEntry {
    fn drop(&mut self) {
        // The capture callback may outlive the entry briefly; end the
        // subscriber streams now rather than when nokhwa drops it.
        self.fanout.close();
    }
}

impl SessionEntry {
    /// Latest captured buffer plus the pixel format the session delivers.
    fn last_frame(&self) -> Result<(nokhwa::Buffer, PixelFormat), CameraError> {
        let buffer = self
            .camera
            .last_frame()
            .map_err(|err| CameraError::Backend(err.to_string()))?;
        Ok((buffer, self.pixel_format))
    }

    /// Best-effort push of `controls` to the device; unsupported knobs
    /// are ignored by the driver.
    fn apply_controls(&mut self, controls: &CameraControls) {
        if let Some(zoom) = controls.zoom {
            // Convert to nokhwa integer zoom. Many V4L2 drivers expose
            // digital zoom as an integer step; UVC devices expose a
            // percentage. Best-effort: scale `zoom` into the control's
            // native range.
            let _ = self.camera.set_camera_control(
                KnownCameraControl::Zoom,
                ControlValueSetter::Integer(control_step(zoom)),
            );
        }
        if let Some(ev) = controls.exposure_compensation_ev {
            let _ = self.camera.set_camera_control(
                KnownCameraControl::Exposure,
                ControlValueSetter::Integer(control_step(ev)),
            );
        }
        if let Some(FocusMode::Manual(distance)) = controls.focus {
            // Manual focus — drivers expose this as an integer lens
            // position. Scale `distance` ([0.0, 1.0]) into i64 — the
            // caller still needs to probe the valid range for best
            // results.
            let scaled = control_step(distance.clamp(0.0, 1.0) * 1000.0);
            let _ = self.camera.set_camera_control(
                KnownCameraControl::Focus,
                ControlValueSetter::Integer(scaled),
            );
        }
        // Torch / flash / white-balance / autofocus coverage varies
        // per camera; leave untouched here.
    }
}

impl DesktopCamera {
    #[must_use]
    pub fn new(config: CameraInitConfig) -> Self {
        Self {
            config,
            sessions: Arc::new(Mutex::new(HashMap::new())),
            runtime: OnceLock::new(),
        }
    }

    /// Runtime this host serves, installing the release hook on first use.
    ///
    /// The hook closes a session when the client drops its
    /// [`crate::SessionHandle`] without calling `close`. The runtime only
    /// forwards that release to its hooks; there is no transport pump to
    /// route it back here.
    fn runtime(&self) -> Result<Arc<Runtime>, CameraError> {
        let weak = self.runtime.get_or_init(|| {
            let runtime = Runtime::global().ok()?;
            let sessions: Weak<Mutex<HashMap<NativeHandleId, SessionEntry>>> =
                Arc::downgrade(&self.sessions);
            runtime.install_native_handle_release_hook(Arc::new(move |id| {
                // Every release in the runtime reaches every hook. Ids this
                // host never handed out are not in the map and fall through.
                if !id.is_rust_allocated() {
                    return;
                }
                let Some(sessions) = sessions.upgrade() else {
                    return;
                };
                let entry = lock(&sessions).remove(&id);
                // Dropping the entry stops the capture thread and frees the device.
                drop(entry);
            }));
            Some(Arc::downgrade(&runtime))
        });
        weak.as_ref()
            .and_then(Weak::upgrade)
            .ok_or_else(|| CameraError::Backend("istmo runtime is not running".to_owned()))
    }

    const fn pool_depth(&self) -> usize {
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
        let infos = query(ApiBackend::Auto).map_err(|err| CameraError::Backend(err.to_string()))?;
        Ok(infos.iter().map(describe_device).collect())
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
        if !matches!(config.pixel_format, PixelFormat::Rgba8 | PixelFormat::Bgra8) {
            return Err(CameraError::FormatUnsupported);
        }

        let fanout = Arc::new(FrameFanout::new(self.pool_depth()));
        let seq = AtomicU64::new(0);
        let pixel_format = config.pixel_format;

        // Resolve the id allocator before touching the device so a missing
        // runtime fails without opening (and leaking) a capture stream.
        let id = self.runtime()?.alloc_native_handle_id();
        let index = resolve_index(&config.device_id);
        let requested = requested_format(&config);

        let publisher = Arc::clone(&fanout);
        let mut camera = CallbackCamera::new(index, requested, move |buffer| {
            match buffer_to_frame(&buffer, pixel_format, seq.fetch_add(1, Ordering::Relaxed)) {
                Ok(frame) => publisher.publish(frame),
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

        lock(&self.sessions).insert(
            id,
            SessionEntry {
                camera,
                fanout,
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
        let mut entry = lock(&self.sessions).remove(&session);
        if let Some(ref mut e) = entry {
            let _ = e.camera.stop_stream();
        }
        Ok(())
    }

    fn frames(&self, session: NativeHandleId) -> flume::Receiver<CameraFrame> {
        lock(&self.sessions).get(&session).map_or_else(
            || flume::bounded::<CameraFrame>(0).1,
            |entry| entry.fanout.subscribe(),
        )
    }

    async fn apply_controls(
        &self,
        session: NativeHandleId,
        controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        lock(&self.sessions)
            .get_mut(&session)
            .ok_or(CameraError::SessionClosed)?
            .apply_controls(&controls);

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
        let (buffer, pixel_format) = lock(&self.sessions)
            .get_mut(&session)
            .ok_or(CameraError::SessionClosed)?
            .last_frame()?;
        let frame = buffer_to_frame(&buffer, pixel_format, 0).map_err(CameraError::Backend)?;
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

fn describe_device(info: &nokhwa::utils::CameraInfo) -> CameraDevice {
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

fn resolve_index(device_id: &str) -> CameraIndex {
    device_id.parse::<u32>().map_or_else(
        |_| CameraIndex::String(device_id.to_owned()),
        CameraIndex::Index,
    )
}

fn requested_format(config: &SessionConfig) -> RequestedFormat<'static> {
    let kind = match (config.resolution, config.target_fps) {
        (Some(res), Some(fps)) => {
            RequestedFormatType::Closest(nokhwa::utils::CameraFormat::new_from(
                res.width,
                res.height,
                nokhwa::utils::FrameFormat::YUYV,
                fps,
            ))
        }
        (Some(_), None) | (None, _) => RequestedFormatType::AbsoluteHighestFrameRate,
    };
    RequestedFormat::new::<RgbAFormat>(kind)
}

// -------------------------------------------------------------------
// Buffer → CameraFrame conversion
// -------------------------------------------------------------------

/// Decode `buffer` into a frame numbered `seq`. `dropped_before` is
/// left at zero; the fan-out fills it per subscriber.
fn buffer_to_frame(
    buffer: &nokhwa::Buffer,
    pixel_format: PixelFormat,
    seq: u64,
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
        .map_or(0, |d| u64::try_from(d.as_nanos()).unwrap_or(u64::MAX));

    Ok(CameraFrame {
        seq,
        timestamp_ns,
        resolution: Resolution { width, height },
        rotation_degrees: 0,
        pixel_format,
        planes: vec![plane],
        bytes,
        dropped_before: 0,
    })
}

/// Swap R and B channels in a packed 32-bit RGBA buffer. Used to
/// convert the RGBA output nokhwa gives us into BGRA when the caller
/// asked for [`PixelFormat::Bgra8`].
fn swap_rb_in_place(bytes: &mut [u8]) {
    for px in bytes.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
}

/// Convert a float control value into the integer step nokhwa expects.
#[expect(
    clippy::cast_possible_truncation,
    reason = "controls take integer steps, so the fraction is dropped on purpose; float-to-int `as` saturates"
)]
const fn control_step(value: f32) -> i64 {
    value as i64
}

fn apply_initial_controls(camera: &mut CallbackCamera, controls: &CameraControls) {
    if let Some(zoom) = controls.zoom {
        let _ = camera.set_camera_control(
            KnownCameraControl::Zoom,
            ControlValueSetter::Integer(control_step(zoom)),
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
