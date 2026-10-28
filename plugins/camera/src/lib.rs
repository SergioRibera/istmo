//! Camera plugin for [`istmo`](https://docs.rs/istmo).
//!
//! Enumerate cameras, stream raw frames in real time, drive focus /
//! zoom / torch / exposure controls, and capture stills — all behind a
//! single typed Rust surface that compiles against Android (CameraX)
//! and iOS (AVFoundation) backends.
//!
//! # Design
//!
//! Camera access is **session-oriented**. Each call to
//! [`CameraClient::open_session`] returns a [`SessionHandle`] whose
//! lifetime owns a native [`NativeHandle`](istmo_core::NativeHandle);
//! dropping the handle releases the native session deterministically.
//! Opening more than one session concurrently is the same code path
//! — multi-camera support is a runtime property reported by
//! [`Camera::concurrent_support`] (iOS A12+ exposes
//! `AVCaptureMultiCamSession`, Android 11+ exposes
//! `CameraX::ConcurrentCamera`).
//!
//! # Real-time frames
//!
//! [`SessionHandle::frames`] returns a `flume::Receiver<CameraFrame>`
//! fed by the native sample-buffer callback. The native side keeps a
//! bounded ring buffer sized by [`CameraInitConfig::frame_pool_depth`]
//! and drops the oldest frame when the consumer falls behind — each
//! delivered frame reports how many predecessors were dropped via
//! [`CameraFrame::dropped_before`]. Pick [`PixelFormat::Nv21`] /
//! [`PixelFormat::Yuv420`] for cheap pass-through, or
//! [`PixelFormat::Jpeg`] when the receiver wants compressed bytes.
//!
//! # Permissions
//!
//! The plugin owns the platform access prompt — call
//! [`CameraClient::request_access`] before [`CameraClient::open_session`]
//! and gate the flow on the returned [`AccessStatus`]. Apps still
//! need `NSCameraUsageDescription` in `Info.plist`; the plugin's
//! `istmo.toml` ships a default string that apps are expected to
//! reword via `[app.info_plist]`.
//!
//! Enable the `codegen` feature to expose the plugin's `Contract`
//! (see [`istmo-build`](https://docs.rs/istmo-build)) without going
//! through the build-script handover.

#![doc(html_root_url = "https://docs.rs/istmo-camera")]

#[cfg(feature = "codegen")]
pub mod codegen;

pub mod desktop;

#[cfg(all(
    any(target_os = "linux", target_os = "macos", target_os = "windows"),
    feature = "desktop"
))]
pub use desktop::{DesktopCamera, DesktopCameraFactory};

use std::sync::Arc;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId, TypedStream};
use istmo_macros::{message, owned, plugin, stream};

/// Wire identifier for the camera plugin.
pub const CAMERA_PLUGIN_ID: &str = "istmo.camera";

/// Phantom tag for the opaque native camera session handle.
#[non_exhaustive]
#[derive(Debug)]
pub enum CameraSessionTag {}

// -------------------------------------------------------------------
// Init / runtime config
// -------------------------------------------------------------------

/// Runtime-wide settings for the camera plugin.
///
/// Shared across every session opened by the same client. Prefer the
/// defaults unless you need to tune backpressure for a specific
/// capture pipeline (recording vs. preview vs. ML inference).
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraInitConfig {
    /// How many frames the native ring buffer may hold before the
    /// oldest is dropped. `0` falls back to the platform default
    /// (`3` — tuned so a 30 Hz consumer sees <100 ms of latency when
    /// it stalls).
    pub frame_pool_depth: u32,
}

impl Default for CameraInitConfig {
    fn default() -> Self {
        Self {
            frame_pool_depth: 3,
        }
    }
}

// -------------------------------------------------------------------
// Discovery
// -------------------------------------------------------------------

/// Lens orientation relative to the device chassis.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CameraFacing {
    Front,
    Back,
    External,
    Unknown,
}

/// Optical characterisation of the lens. Normalised across platforms:
/// Android's `LENS_INFO_AVAILABLE_FOCAL_LENGTHS` + `REQUEST_AVAILABLE_CAPABILITIES`
/// map to the same discriminants iOS exposes via
/// `AVCaptureDevice.DeviceType`.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LensKind {
    /// Primary wide-angle lens — the "default" camera most apps mean.
    Wide,
    /// Ultra-wide (≤ 20 mm equivalent on iOS, matching physical
    /// cameras on Android when `focal_lengths_mm` is noticeably
    /// shorter than the primary).
    UltraWide,
    /// Telephoto lens — any additional back-side camera with a focal
    /// length significantly longer than `Wide`.
    Telephoto,
    /// Macro-capable lens (dedicated or shared, depending on vendor).
    Macro,
    /// iOS TrueDepth front camera (also surfaces `supports_depth`).
    TrueDepth,
    /// LiDAR scanner (iPhone Pro / iPad Pro). Companion to a
    /// photographic camera; see `physical_sensors`.
    Lidar,
    /// Logical camera that stitches multiple physical cameras (iOS
    /// dual / triple / dual-wide; Android logical multi-cam).
    Logical,
    Unknown,
}

/// Pixel resolution. Dimensions are nominal — ask the session for the
/// `negotiated` resolution after `open_session`, since the backend may
/// coerce to the closest supported mode.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Resolution {
    pub width: u32,
    pub height: u32,
}

/// A `(resolution, pixel_format, fps_range)` triple the backend will
/// accept for a session. The driver can usually produce other
/// combinations by downscaling / colour-converting, but these are the
/// *native* modes — picking one avoids a hidden conversion step.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SupportedFormat {
    pub resolution: Resolution,
    pub pixel_format: PixelFormat,
    pub min_fps: u32,
    pub max_fps: u32,
}

/// Standardised capability flags. Fields map one-to-one to the
/// equivalent `CameraCharacteristics` / `AVCaptureDevice` queries so
/// shared Rust code can gate features without `cfg(target_os = …)`.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct CameraCapabilities {
    pub min_zoom: f32,
    pub max_zoom: f32,
    /// Logical zoom steps that correspond to a physical lens switch —
    /// e.g. `[0.5, 1.0, 2.0]` on a triple-cam iPhone. Empty when the
    /// camera is single-lens.
    pub optical_zoom_steps: Vec<f32>,
    /// Reported focal lengths in millimetres. Multiple entries for
    /// logical multi-cam.
    pub focal_lengths_mm: Vec<f32>,
    pub supports_flash: bool,
    pub supports_torch: bool,
    pub supports_autofocus: bool,
    pub supports_manual_focus: bool,
    pub supports_ois: bool,
    pub supports_eis: bool,
    pub supports_hdr: bool,
    pub supports_depth: bool,
    pub supports_raw: bool,
    pub max_still_resolution: Resolution,
}

/// Full description of a camera. Returned by [`Camera::devices`].
///
/// `id` is opaque — the platform's own camera identifier (Android
/// `CameraManager` id string, iOS `AVCaptureDevice.uniqueID`). Pass it
/// unmodified to [`SessionConfig::device_id`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct CameraDevice {
    pub id: String,
    /// Human-readable name — "Back Dual Wide Camera", "Front Camera",
    /// etc. Suitable for user-facing selectors.
    pub label: String,
    pub facing: CameraFacing,
    pub lens: LensKind,
    /// Physical camera ids this logical camera composes. Empty for
    /// single-sensor cameras.
    pub physical_sensors: Vec<String>,
    pub capabilities: CameraCapabilities,
    pub supported_formats: Vec<SupportedFormat>,
}

/// A pair of camera ids the platform guarantees it can stream
/// concurrently. Mirrors Android's `CameraManager::getConcurrentCameraIds`.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ConcurrentPair {
    pub primary: String,
    pub secondary: String,
}

/// Platform-advertised concurrent-camera limits.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConcurrentSupport {
    /// Maximum number of sessions the platform will keep open at
    /// once. `1` means multi-cam is not supported.
    pub max_concurrent: u32,
    /// Specific `(primary, secondary)` combinations the vendor
    /// guarantees. Non-empty on Android ≥ 11; empty where the
    /// platform allows any pairing.
    pub approved_pairs: Vec<ConcurrentPair>,
    /// `true` when the platform supports arbitrary combinations up to
    /// `max_concurrent` (iOS `AVCaptureMultiCamSession` on A12+).
    pub any_combination: bool,
}

// -------------------------------------------------------------------
// Pixel formats
// -------------------------------------------------------------------

/// Wire pixel format for streamed frames and still captures.
///
/// Prefer the native YUV formats for cheap pass-through; the backend
/// avoids a colour-conversion pass. Pick [`PixelFormat::Jpeg`] when
/// the consumer wants compressed bytes (e.g. shipping over a network
/// socket) — the backend asks the hardware encoder when available.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PixelFormat {
    /// Android `ImageFormat::NV21` — Y plane + interleaved VU plane.
    /// Default on Android.
    Nv21,
    /// Planar / semi-planar 4:2:0. On Android this maps to
    /// `YUV_420_888` (three planes); on iOS to
    /// `kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange`.
    Yuv420,
    /// 32-bit BGRA (iOS default when YUV is unavailable).
    Bgra8,
    /// 32-bit RGBA.
    Rgba8,
    /// Encoded JPEG. The payload is the hardware encoder's quality
    /// knob (`0..=100`); 70 is a typical sweet spot.
    Jpeg(u8),
}

// -------------------------------------------------------------------
// Permissions
// -------------------------------------------------------------------

/// Current camera-access authorisation state.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AccessStatus {
    Granted,
    Denied,
    /// User has not been asked yet. Call
    /// [`CameraClient::request_access`] to prompt.
    NotDetermined,
    /// Parental controls / MDM / enterprise policy forbids access.
    /// A permission prompt will not help.
    Restricted,
}

// -------------------------------------------------------------------
// Session configuration
// -------------------------------------------------------------------

/// What the caller wants from a session.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct SessionConfig {
    pub device_id: String,
    pub pixel_format: PixelFormat,
    /// `None` → backend picks the camera's preferred mode. Pass a
    /// value matched to a `SupportedFormat` to pin the native mode
    /// and skip conversion.
    pub resolution: Option<Resolution>,
    /// Target frame rate. `None` → backend picks the max for the
    /// chosen resolution. The backend may clamp to the closest
    /// supported range.
    pub target_fps: Option<u32>,
    /// `false` → the session is preview-only; no frames are produced
    /// over the stream. Useful when driving an on-device preview
    /// surface through another path.
    pub emit_frames: bool,
    pub initial_controls: CameraControls,
}

impl SessionConfig {
    /// Starts a `SessionConfig` for `device_id` streaming NV21 at
    /// the camera's preferred mode. Flip knobs via the dedicated
    /// setters.
    #[must_use]
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            pixel_format: PixelFormat::Nv21,
            resolution: None,
            target_fps: None,
            emit_frames: true,
            initial_controls: CameraControls::default(),
        }
    }

    #[must_use]
    pub const fn with_pixel_format(mut self, format: PixelFormat) -> Self {
        self.pixel_format = format;
        self
    }

    #[must_use]
    pub const fn with_resolution(mut self, resolution: Resolution) -> Self {
        self.resolution = Some(resolution);
        self
    }

    #[must_use]
    pub const fn with_target_fps(mut self, fps: u32) -> Self {
        self.target_fps = Some(fps);
        self
    }

    #[must_use]
    pub const fn preview_only(mut self) -> Self {
        self.emit_frames = false;
        self
    }

    #[must_use]
    pub fn with_initial_controls(mut self, controls: CameraControls) -> Self {
        self.initial_controls = controls;
        self
    }
}

/// Negotiated capture parameters — what the backend actually opened,
/// which may differ from [`SessionConfig`] when the requested mode
/// wasn't an exact native match.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct NegotiatedFormat {
    pub resolution: Resolution,
    pub pixel_format: PixelFormat,
    pub fps: u32,
    pub rotation_degrees: u16,
}

/// Wire response from [`Camera::open`]. Prefer
/// [`CameraClient::open_session`] on the client, which wraps this in
/// a [`SessionHandle`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraSession {
    #[handle(CameraSessionTag)]
    pub session: NativeHandleId,
    pub negotiated: NegotiatedFormat,
}

// -------------------------------------------------------------------
// Frames
// -------------------------------------------------------------------

/// Row / plane layout of a single plane in a multi-planar pixel
/// format. For planar YUV, there are three entries (Y, U, V); for
/// semi-planar NV21 / NV12 there are two (Y, VU / UV); for packed
/// RGBA / JPEG there is one.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct FramePlane {
    /// Byte offset within [`CameraFrame::bytes`] where this plane
    /// starts.
    pub offset: u32,
    /// Row stride in bytes. May exceed `width * pixel_stride` when
    /// the hardware aligns rows.
    pub row_stride: u32,
    /// Pixel stride in bytes between consecutive samples on the same
    /// row. `1` for Y planes, `2` for interleaved UV, `4` for RGBA.
    pub pixel_stride: u32,
    pub width: u32,
    pub height: u32,
}

/// One frame delivered on the capture stream.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CameraFrame {
    /// Monotonically increasing sequence number since the session
    /// opened. Combine with `dropped_before` to reconstruct the
    /// camera's own frame index.
    pub seq: u64,
    /// Capture timestamp in nanoseconds, in the platform's monotonic
    /// clock (`SystemClock.elapsedRealtimeNanos` on Android,
    /// `CMSampleBufferGetPresentationTimeStamp` on iOS).
    pub timestamp_ns: u64,
    pub resolution: Resolution,
    /// Clockwise rotation (0 / 90 / 180 / 270) the receiver should
    /// apply to display the frame upright.
    pub rotation_degrees: u16,
    pub pixel_format: PixelFormat,
    pub planes: Vec<FramePlane>,
    pub bytes: Vec<u8>,
    /// How many frames the backend dropped since the previous
    /// delivery. `0` on a healthy consumer. Non-zero → the consumer
    /// is behind the producer; consider a cheaper
    /// [`PixelFormat`] / lower resolution / lower `target_fps`.
    pub dropped_before: u32,
}

// -------------------------------------------------------------------
// Controls
// -------------------------------------------------------------------

/// Focus strategy applied to a running session.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FocusMode {
    /// Single-shot autofocus (returns to idle after locking).
    Auto,
    /// Continuous autofocus optimised for video.
    ContinuousVideo,
    /// Continuous autofocus optimised for stills.
    ContinuousPicture,
    /// Lock focus at infinity (or current position where unsupported).
    Fixed,
    /// Manual focus — lens distance in the `[0.0, 1.0]` range (0 =
    /// closest, 1 = infinity). Requires `supports_manual_focus`.
    Manual(f32),
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WhiteBalanceMode {
    Auto,
    Daylight,
    Cloudy,
    Incandescent,
    Fluorescent,
    Shade,
    Twilight,
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FlashMode {
    Off,
    On,
    Auto,
}

/// Mutable session controls. Fields are `Option` so callers can
/// change one knob without having to re-send the rest — pass `None`
/// to leave a setting untouched. The backend returns the effective
/// state as [`AppliedControls`].
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CameraControls {
    pub zoom: Option<f32>,
    pub torch: Option<bool>,
    pub focus: Option<FocusMode>,
    pub exposure_compensation_ev: Option<f32>,
    pub white_balance: Option<WhiteBalanceMode>,
    pub flash: Option<FlashMode>,
}

/// Effective state after the backend applied a [`CameraControls`]
/// request. Each field reflects the actual value on the running
/// session, which may differ from the request when the hardware
/// clamps.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedControls {
    pub zoom: f32,
    pub torch: bool,
    pub focus: FocusMode,
    pub exposure_compensation_ev: f32,
    pub white_balance: WhiteBalanceMode,
    pub flash: FlashMode,
}

// -------------------------------------------------------------------
// Still capture
// -------------------------------------------------------------------

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StillCaptureRequest {
    pub pixel_format: PixelFormat,
    pub resolution: Option<Resolution>,
    pub flash: FlashMode,
}

impl Default for StillCaptureRequest {
    fn default() -> Self {
        Self {
            pixel_format: PixelFormat::Jpeg(90),
            resolution: None,
            flash: FlashMode::Auto,
        }
    }
}

#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct StillPhoto {
    pub resolution: Resolution,
    pub rotation_degrees: u16,
    pub pixel_format: PixelFormat,
    pub bytes: Vec<u8>,
    pub timestamp_ns: u64,
}

// -------------------------------------------------------------------
// Errors
// -------------------------------------------------------------------

/// Domain-level errors surfaced by every [`Camera`] method.
#[message(bincode = "::bincode", crate = "::istmo_core")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CameraError {
    /// User denied camera access, or the OS declined before the
    /// prompt (`AccessStatus::Restricted`).
    PermissionDenied,
    /// No camera with that id. Devices can disappear between
    /// `devices()` and `open()` on hot-plug platforms.
    DeviceNotFound(String),
    /// Requested [`PixelFormat`] / resolution / FPS triple is
    /// outside what the device advertised.
    FormatUnsupported,
    /// Caller tried to open more concurrent sessions than
    /// [`ConcurrentSupport::max_concurrent`] permits.
    ConcurrencyUnsupported,
    /// Session id does not correspond to a live session (already
    /// closed, or the handle was released).
    SessionClosed,
    /// Capture was interrupted by the OS (phone call, resource
    /// reclamation on iOS, process restart). Reopen the session.
    Interrupted,
    /// Native backend reported a vendor error. Message is the raw
    /// platform string.
    Backend(String),
}

impl std::fmt::Display for CameraError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::PermissionDenied => f.write_str("camera permission denied"),
            Self::DeviceNotFound(id) => write!(f, "camera `{id}` not found"),
            Self::FormatUnsupported => f.write_str("requested capture format is not supported"),
            Self::ConcurrencyUnsupported => {
                f.write_str("too many concurrent camera sessions for this device")
            }
            Self::SessionClosed => f.write_str("camera session is already closed"),
            Self::Interrupted => f.write_str("camera capture was interrupted"),
            Self::Backend(msg) => write!(f, "camera backend error: {msg}"),
        }
    }
}

impl std::error::Error for CameraError {}

impl From<IstmoError> for CameraError {
    fn from(err: IstmoError) -> Self {
        if let IstmoError::PluginError { ref bytes } = err {
            if let Ok((decoded, _)) =
                bincode::decode_from_slice::<Self, _>(bytes, bincode::config::standard())
            {
                return decoded;
            }
        }
        Self::Backend(err.to_string())
    }
}

// -------------------------------------------------------------------
// Plugin trait
// -------------------------------------------------------------------

/// Rust-facing camera surface. The native backend implements the
/// same trait on Android (Kotlin, CameraX) and iOS (Swift,
/// AVFoundation); see the plugin's `native/` directory for reference
/// implementations.
#[plugin(
    name = "istmo.camera",
    init = CameraInitConfig,
    crate = "::istmo_core",
)]
pub trait Camera {
    /// Enumerate every camera the OS currently exposes. Cheap on
    /// Android (static query), noticeably slower on iOS the first
    /// time (triggers `AVCaptureDevice.DiscoverySession`).
    async fn devices(&self) -> Result<Vec<CameraDevice>, CameraError>;

    /// Report how many sessions may run concurrently and which
    /// vendor-blessed pairings are safe.
    async fn concurrent_support(&self) -> Result<ConcurrentSupport, CameraError>;

    /// Current authorisation state. Non-blocking; does not prompt.
    async fn access_status(&self) -> Result<AccessStatus, CameraError>;

    /// Prompt the user for access when the current status is
    /// `NotDetermined`. Returns the status the OS settled on. On
    /// Android this launches `ActivityResultContracts.RequestPermission`;
    /// on iOS it awaits `AVCaptureDevice.requestAccess(for: .video)`.
    async fn request_access(&self) -> Result<AccessStatus, CameraError>;

    /// Open a session. The returned [`CameraSession::session`] handle
    /// owns the native capture pipeline — the macro-generated
    /// `open_owned` sibling wraps it in a [`NativeHandle`] whose drop
    /// closes the session.
    #[owned]
    async fn open(&self, config: SessionConfig) -> Result<CameraSession, CameraError>;

    /// Explicitly close a session. Prefer dropping the owned
    /// [`NativeHandle<CameraSessionTag>`] from [`CameraSession::session`];
    /// the backend fires the same release path.
    async fn close(&self, session: NativeHandleId) -> Result<(), CameraError>;

    /// Live frame stream for a session. The receiver closes when the
    /// session is closed or the plugin dropped. See
    /// [`CameraFrame::dropped_before`] for backpressure reporting.
    #[stream]
    fn frames(&self, session: NativeHandleId) -> CameraFrame;

    /// Apply (or clear) one or more capture controls. Returns the
    /// effective state after the backend coerced out-of-range values.
    async fn apply_controls(
        &self,
        session: NativeHandleId,
        controls: CameraControls,
    ) -> Result<AppliedControls, CameraError>;

    /// Capture a single still. The returned [`StillPhoto::bytes`]
    /// uses the pixel format requested in [`StillCaptureRequest`].
    async fn capture_still(
        &self,
        session: NativeHandleId,
        request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError>;
}

// -------------------------------------------------------------------
// Ergonomic client surface
// -------------------------------------------------------------------

/// Owned camera session. Wraps the generated client plus the native
/// handle so callers can drive the session with method sugar instead
/// of threading `NativeHandleId` through every call.
///
/// Dropping the handle releases the native session.
#[derive(Debug)]
pub struct SessionHandle {
    client: Arc<CameraClient>,
    session: NativeHandle<CameraSessionTag>,
    negotiated: NegotiatedFormat,
}

impl SessionHandle {
    /// Negotiated capture parameters as reported by the backend when
    /// the session opened.
    #[must_use]
    pub const fn negotiated(&self) -> &NegotiatedFormat {
        &self.negotiated
    }

    /// Opaque native handle id. Useful for lower-level routing (e.g.
    /// a sibling plugin that consumes the same session).
    #[must_use]
    pub fn id(&self) -> NativeHandleId {
        self.session.id()
    }

    /// Subscribe to the capture stream.
    ///
    /// Calling this twice on the same session yields two independent
    /// receivers — the backend fans frames out to every subscriber.
    ///
    /// # Errors
    /// [`CameraError::SessionClosed`] when the native session has
    /// been released, or any transport-level failure wrapped as
    /// [`CameraError::Backend`].
    pub fn frames(&self) -> Result<TypedStream<CameraFrame>, CameraError> {
        self.client.frames(self.session.id()).map_err(CameraError::from)
    }

    /// Apply control changes. See [`Camera::apply_controls`].
    ///
    /// # Errors
    /// Propagates [`CameraError`] from the backend.
    pub async fn apply_controls(
        &self,
        controls: CameraControls,
    ) -> Result<AppliedControls, CameraError> {
        self.client
            .apply_controls(self.session.id(), controls)
            .await
            .map_err(CameraError::from)
    }

    /// Capture a still from this session. See [`Camera::capture_still`].
    ///
    /// # Errors
    /// Propagates [`CameraError`] from the backend.
    pub async fn capture_still(
        &self,
        request: StillCaptureRequest,
    ) -> Result<StillPhoto, CameraError> {
        self.client
            .capture_still(self.session.id(), request)
            .await
            .map_err(CameraError::from)
    }

    /// Close the session explicitly. Equivalent to dropping the
    /// handle, but returns the backend's close result instead of
    /// swallowing it.
    ///
    /// # Errors
    /// Propagates [`CameraError`] from the backend.
    pub async fn close(self) -> Result<(), CameraError> {
        let id = self.session.id();
        // Consume the native handle so its drop path doesn't fire a
        // second release.
        let _: NativeHandleId = self.session.into_id();
        self.client.close(id).await.map_err(CameraError::from)
    }
}

/// Extension trait so [`CameraClient`] gets ergonomic session
/// helpers without needing a `Clone` impl on the macro-generated
/// struct — callers wrap the client in `Arc` and these helpers
/// clone the `Arc`, not the inner state.
pub trait CameraClientExt {
    /// Open a session and wrap it in an ergonomic [`SessionHandle`].
    ///
    /// # Errors
    /// Propagates [`CameraError`] from the backend. Common failures:
    /// [`CameraError::PermissionDenied`], [`CameraError::DeviceNotFound`],
    /// [`CameraError::FormatUnsupported`],
    /// [`CameraError::ConcurrencyUnsupported`].
    fn open_session(
        &self,
        config: SessionConfig,
    ) -> impl std::future::Future<Output = Result<SessionHandle, CameraError>> + Send;

    /// Convenience `(device_id, pixel_format)` opener. Everything
    /// else keeps the backend defaults.
    ///
    /// # Errors
    /// See [`CameraClientExt::open_session`].
    fn open_simple(
        &self,
        device_id: String,
        pixel_format: PixelFormat,
    ) -> impl std::future::Future<Output = Result<SessionHandle, CameraError>> + Send;
}

impl CameraClientExt for Arc<CameraClient> {
    async fn open_session(
        &self,
        config: SessionConfig,
    ) -> Result<SessionHandle, CameraError> {
        let session = self.open_owned(config).await.map_err(CameraError::from)?;
        Ok(SessionHandle {
            client: Arc::clone(self),
            session: session.session,
            negotiated: session.negotiated,
        })
    }

    async fn open_simple(
        &self,
        device_id: String,
        pixel_format: PixelFormat,
    ) -> Result<SessionHandle, CameraError> {
        self.open_session(SessionConfig::new(device_id).with_pixel_format(pixel_format))
            .await
    }
}
