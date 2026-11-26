//! The Rust desktop host: backend registry, session table, origin
//! checks.
//!
//! ```no_run
//! use istmo_screen_capture::{CapturePolicy, DesktopCapture, ScreenCaptureHost};
//! # fn register(runtime: &std::sync::Arc<istmo_core::Runtime>) {
//! runtime.register_host(ScreenCaptureHost::new(DesktopCapture::with_policy(CapturePolicy::default())));
//! # }
//! ```

mod backend;
mod clock;
mod engine;
#[cfg(test)]
mod fake;
#[cfg(target_os = "linux")]
mod linux;
mod offload;
mod output;
#[cfg(test)]
mod tests;

use std::sync::Arc;

use istmo_core::{CallOrigin, FrameSinkId, HostCtx, NativeHandleId};

use crate::ScreenCapture;
use crate::policy::CapturePolicy;
use crate::types::{
    AccessRequest, AccessScope, AccessState, AudioMeta, BackendStatus, CapabilityQuery,
    CapabilityReport, CaptureError, CaptureEvent, CaptureSession, FrameMeta, NegotiatedStream,
    PixelLayout, PreviewTarget, ProjectionGrant, ProjectionRequest, RawImage, Requirement,
    ScreenshotInfo, ScreenshotRequest, SessionConfig, SessionUpdate, SourceFilter, SourceList,
};

use self::engine::Engine;
use self::offload::offload;

/// The desktop host. Register it with
/// `ScreenCaptureHost::new(DesktopCapture::default())`.
#[derive(Debug, Clone)]
pub struct DesktopCapture {
    engine: Arc<Engine>,
}

impl Default for DesktopCapture {
    fn default() -> Self {
        Self::with_policy(CapturePolicy::default())
    }
}

impl DesktopCapture {
    /// The compiled backends under `policy`.
    #[must_use]
    pub fn with_policy(policy: CapturePolicy) -> Self {
        Self {
            engine: Arc::new(Engine::platform(policy)),
        }
    }

    #[cfg(test)]
    fn from_engine(engine: Engine) -> Self {
        Self {
            engine: Arc::new(engine),
        }
    }

    /// Same-process callers always; remote ones only for methods the
    /// policy opened.
    fn admit(&self, ctx: &HostCtx, method: &'static str) -> Result<(), CaptureError> {
        match ctx.origin() {
            CallOrigin::Local | CallOrigin::Peer => Ok(()),
            CallOrigin::Remote if self.engine.policy().admits_remote(method) => Ok(()),
            // Remote, and any origin added later: refused by default.
            _ => Err(CaptureError::OriginRefused),
        }
    }

    fn engine(&self) -> Arc<Engine> {
        Arc::clone(&self.engine)
    }
}

impl ScreenCapture for DesktopCapture {
    async fn backends(&self) -> Vec<BackendStatus> {
        let engine = self.engine();
        offload("backends", move || Ok(engine.statuses()))
            .await
            .unwrap_or_default()
    }

    async fn capabilities(&self, query: CapabilityQuery) -> Result<CapabilityReport, CaptureError> {
        let engine = self.engine();
        offload("capabilities", move || engine.capabilities(&query)).await
    }

    async fn sources(
        &self,
        ctx: HostCtx,
        filter: SourceFilter,
    ) -> Result<SourceList, CaptureError> {
        self.admit(&ctx, "sources")?;
        let engine = self.engine();
        offload("sources", move || engine.sources(&filter)).await
    }

    async fn access(&self, scope: AccessScope) -> Result<AccessState, CaptureError> {
        Ok(Engine::access(scope))
    }

    async fn request_access(
        &self,
        ctx: HostCtx,
        request: AccessRequest,
    ) -> Result<AccessState, CaptureError> {
        self.admit(&ctx, "request_access")?;
        // Desktop Linux has no standing grant to prompt for.
        Ok(Engine::access(request.scope))
    }

    async fn request_projection(
        &self,
        ctx: HostCtx,
        _request: ProjectionRequest,
    ) -> Result<ProjectionGrant, CaptureError> {
        self.admit(&ctx, "request_projection")?;
        Err(CaptureError::unsatisfiable(None, Requirement::Operation))
    }

    async fn screenshot(
        &self,
        ctx: HostCtx,
        request: ScreenshotRequest,
        sink: FrameSinkId<FrameMeta>,
    ) -> Result<ScreenshotInfo, CaptureError> {
        self.admit(&ctx, "screenshot")?;
        let sender = ctx
            .frame_sender(sink)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("screenshot", move || {
            engine.screenshot_into(&request, &sender, origin)
        })
        .await
    }

    async fn screenshot_pixels(
        &self,
        ctx: HostCtx,
        request: ScreenshotRequest,
        layout: PixelLayout,
    ) -> Result<RawImage, CaptureError> {
        self.admit(&ctx, "screenshot_pixels")?;
        let engine = self.engine();
        offload("screenshot", move || {
            engine.screenshot_pixels(&request, layout)
        })
        .await
    }

    async fn open(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        video: FrameSinkId<FrameMeta>,
    ) -> Result<CaptureSession, CaptureError> {
        self.admit(&ctx, "open")?;
        let video = ctx
            .frame_sender(video)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        let runtime = ctx.runtime().map_err(CaptureError::from)?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("open", move || {
            engine.open(&runtime, &config, &video, None, origin)
        })
        .await
    }

    async fn open_with_audio(
        &self,
        ctx: HostCtx,
        config: SessionConfig,
        video: FrameSinkId<FrameMeta>,
        audio: FrameSinkId<AudioMeta>,
    ) -> Result<CaptureSession, CaptureError> {
        self.admit(&ctx, "open_with_audio")?;
        let video = ctx
            .frame_sender(video)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        let audio = ctx
            .frame_sender(audio)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        let runtime = ctx.runtime().map_err(CaptureError::from)?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("open", move || {
            engine.open(&runtime, &config, &video, Some(audio), origin)
        })
        .await
    }

    async fn open_preview(
        &self,
        ctx: HostCtx,
        _config: SessionConfig,
        _target: PreviewTarget,
    ) -> Result<CaptureSession, CaptureError> {
        self.admit(&ctx, "open_preview")?;
        // Desktop consumers render from sinks (DMA-BUF / D3D11 imports);
        // preview surfaces are a mobile / web feature.
        Err(CaptureError::unsatisfiable(None, Requirement::Operation))
    }

    async fn open_preview_with_frames(
        &self,
        ctx: HostCtx,
        _config: SessionConfig,
        _target: PreviewTarget,
        _video: FrameSinkId<FrameMeta>,
    ) -> Result<CaptureSession, CaptureError> {
        self.admit(&ctx, "open_preview_with_frames")?;
        Err(CaptureError::unsatisfiable(None, Requirement::Operation))
    }

    async fn update(
        &self,
        ctx: HostCtx,
        session: NativeHandleId,
        update: SessionUpdate,
    ) -> Result<NegotiatedStream, CaptureError> {
        self.admit(&ctx, "update")?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("update", move || engine.update(session, &update, origin)).await
    }

    async fn close(&self, ctx: HostCtx, session: NativeHandleId) -> Result<(), CaptureError> {
        self.admit(&ctx, "close")?;
        let engine = self.engine();
        offload("close", move || engine.close(session)).await
    }

    fn events(
        &self,
        ctx: HostCtx,
        session: NativeHandleId,
    ) -> flume::Receiver<Result<CaptureEvent, CaptureError>> {
        if let Err(err) = self.admit(&ctx, "events") {
            let (tx, rx) = flume::bounded(1);
            let _queued = tx.try_send(Err(err));
            return rx;
        }
        self.engine.events(session)
    }
}
