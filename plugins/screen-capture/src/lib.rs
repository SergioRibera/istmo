//! Screen capture plugin for [`istmo`](https://docs.rs/istmo).
//!
//! Screenshots and live streams of monitors, windows, applications and
//! regions, with an honest capability report, explicit backend and
//! format selection and consent modelled as state. The control plane is
//! the [`ScreenCapture`] contract; frames travel through native frame
//! sinks ([`FrameSinkId`]) without being serialised, and the plugin
//! never encodes them.
//!
//! # Hosts
//!
//! - Desktop (Rust): [`DesktopCapture`], built from the compiled backend
//!   features (`linux-portal`, `linux-ext-image-copy`,
//!   `linux-wlr-screencopy`, `linux-x11`, `linux-audio`). A build with
//!   no backend for its platform answers
//!   [`CaptureError::NoBackendCompiled`].
//! - Android / iOS: the Kotlin / Swift backends in `native/` (not
//!   capturing yet: they answer `NoBackendCompiled`).
//!
//! # Threads
//!
//! Every method returns a future that completes on any executor; the
//! blocking platform work (Wayland queues, the portal, PipeWire, X11)
//! runs on the plugin's own threads, so no method is
//! `#[istmo::blocking]`.
//!
//! # Security
//!
//! Capture methods answer only callers in this process unless the app
//! opens them through [`CapturePolicy::remote_methods`]; restore tokens
//! never leave the process. The plugin does not gate capture on
//! consent: [`BackendCaps::consent`] and [`NegotiatedStream::consent`]
//! say how each backend asks, so an app that wants consent-backed
//! capture excludes `ConsentModel::NoneNeeded` backends itself.

#![doc(html_root_url = "https://docs.rs/istmo-screen-capture")]

mod client;
pub mod composite;
#[cfg(not(target_arch = "wasm32"))]
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod desktop;
pub mod pixels;
mod policy;
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod pool;
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub(crate)` marks crate-internal items; plain `pub` in private modules trips `unreachable_pub`"
)]
mod select;
mod types;

use istmo_core::{FrameSinkId, HostCtx, NativeHandleId};
use istmo_macros::{owned, plugin, stream};

pub use client::{ScreenCaptureClientExt, SessionHandle, SourceWatch};
#[cfg(not(target_arch = "wasm32"))]
pub use desktop::DesktopCapture;
pub use policy::CapturePolicy;
pub use types::*;

/// Wire identifier of the plugin.
pub const SCREEN_CAPTURE_PLUGIN_ID: &str = "istmo.screen_capture";

/// The screen-capture contract.
///
/// Methods taking a `HostCtx` check the caller's origin: capture
/// methods answer only callers in this process (`Local` Rust, `Peer`
/// Kotlin / Swift of the same app) unless [`CapturePolicy`] opens them;
/// `backends`, `capabilities` and `access` are open to all, and only a
/// few of them run at once (the rest are refused), so remote callers
/// cannot crowd out the app's own calls.
#[plugin(name = "istmo.screen_capture", remote = true, crate = "::istmo_core")]
pub trait ScreenCapture {
    /// Compiled backends, whether each can run here, and why not.
    async fn backends(&self) -> Vec<BackendStatus>;

    /// Capability report for every runnable backend (or one), for a
    /// source kind or a concrete source. Never prompts.
    async fn capabilities(&self, query: CapabilityQuery) -> Result<CapabilityReport, CaptureError>;

    /// Monitors, windows and applications: capturable by id, or listed
    /// only (`Source::capture`, a picker captures those).
    /// Same origin rule as capture (titles and pids are sensitive).
    async fn sources(&self, ctx: HostCtx, filter: SourceFilter)
    -> Result<SourceList, CaptureError>;

    /// The sources `sources` lists, then every change to them: monitors
    /// plugged, unplugged or reconfigured, windows opened, closed,
    /// retitled or refocused. The first item is `Listed`. Same-process
    /// callers only, whatever the policy (titles are sensitive).
    #[stream]
    fn watch_sources(
        &self,
        ctx: HostCtx,
        filter: SourceFilter,
    ) -> Result<SourceChange, CaptureError>;

    /// Standing grants (TCC, Windows borderless, Android notifications),
    /// without prompting.
    async fn access(&self, scope: AccessScope) -> Result<AccessState, CaptureError>;

    /// Prompt for a standing grant. Per-session consent (portal dialog,
    /// pickers, MediaProjection, `getDisplayMedia`) is never asked here:
    /// it happens inside `open` / `screenshot` when `allow_picker` is set.
    async fn request_access(
        &self,
        ctx: HostCtx,
        request: AccessRequest,
    ) -> Result<AccessState, CaptureError>;

    /// Android only: run the MediaProjection consent now and keep the
    /// grant for one later `open` / `screenshot`.
    async fn request_projection(
        &self,
        ctx: HostCtx,
        request: ProjectionRequest,
    ) -> Result<ProjectionGrant, CaptureError>;

    /// One frame (or one per monitor) into `sink`. When this returns
    /// `Ok`, `info.frames` frames are already queued in the sink.
    async fn screenshot(
        &self,
        ctx: HostCtx,
        request: ScreenshotRequest,
        sink: FrameSinkId<FrameMeta>,
    ) -> Result<ScreenshotInfo, CaptureError>;

    /// The same capture as raw pixels over the wire. Never encoded.
    async fn screenshot_pixels(
        &self,
        ctx: HostCtx,
        request: ScreenshotRequest,
        layout: PixelLayout,
    ) -> Result<RawImage, CaptureError>;

    /// A live session, frames into `video`.
    #[owned]
    async fn open(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        video: FrameSinkId<FrameMeta>,
    ) -> Result<CaptureSession, CaptureError>;

    /// A live session with audio into a second sink.
    #[owned]
    async fn open_with_audio(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        video: FrameSinkId<FrameMeta>,
        audio: FrameSinkId<AudioMeta>,
    ) -> Result<CaptureSession, CaptureError>;

    /// A live session rendered straight into a platform surface.
    #[owned]
    async fn open_preview(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        target: PreviewTarget,
    ) -> Result<CaptureSession, CaptureError>;

    /// Preview plus frames from one consent (Android: a declared GPU
    /// fan-out, `CopyStage::PreviewFanOut`).
    #[owned]
    async fn open_preview_with_frames(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        target: PreviewTarget,
        video: FrameSinkId<FrameMeta>,
    ) -> Result<CaptureSession, CaptureError>;

    /// Frame rate, cursor, region, format tiers, pause / resume. Returns
    /// the renegotiated stream, or an error leaving it as it was.
    async fn update(
        &self,
        ctx: HostCtx,
        session: NativeHandleId,
        update: SessionUpdate,
    ) -> Result<NegotiatedStream, CaptureError>;

    async fn close(&self, ctx: HostCtx, session: NativeHandleId) -> Result<(), CaptureError>;

    /// Session events. Events emitted before the first subscriber are
    /// kept (bounded) and replayed to it.
    #[stream]
    fn events(&self, ctx: HostCtx, session: NativeHandleId) -> Result<CaptureEvent, CaptureError>;
}
