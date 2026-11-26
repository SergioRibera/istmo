//! The Rust desktop host: backend registry, session table, origin
//! checks.
//!
//! ```no_run
//! use istmo_screen_capture::{CapturePolicy, DesktopCapture, ScreenCaptureHost};
//! # fn register(runtime: &std::sync::Arc<istmo_core::Runtime>) {
//! runtime.register_host(ScreenCaptureHost::new(DesktopCapture::with_policy(CapturePolicy::default())));
//! # }
//! ```

mod audio_clock;
mod backend;
mod clock;
mod engine;
#[cfg(test)]
mod fake;
#[cfg(target_os = "linux")]
mod linux;
mod offload;
mod output;
#[cfg(any(all(target_os = "macos", feature = "macos-sck"), test))]
mod sck_map;
#[cfg(test)]
mod tests;
mod watch;
#[cfg(all(windows, feature = "windows-common"))]
mod windows;

use std::sync::Arc;

use istmo_core::{CallOrigin, FrameSinkId, HostCtx, NativeHandleId};

use crate::ScreenCapture;
use crate::policy::CapturePolicy;
use crate::types::{
    AccessRequest, AccessScope, AccessState, AudioCaptureSession, AudioConfig, AudioEvent,
    AudioMeta, AudioNegotiated, AudioUpdate, BackendStatus, CapabilityQuery, CapabilityReport,
    CaptureError, CaptureEvent, CaptureSession, FrameMeta, NegotiatedStream, PixelLayout,
    PreviewTarget, ProjectionGrant, ProjectionRequest, RawImage, Requirement, ScreenshotInfo,
    ScreenshotRequest, SessionConfig, SessionUpdate, SourceChange, SourceFilter, SourceList,
};

use self::engine::Engine;
use self::offload::{internal, offload, offload_open, offload_owned};

/// Changes queued per source-watch subscriber before its watcher waits.
const WATCH_DEPTH: usize = 64;

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
        offload_open("backends", move || Ok(engine.statuses()))
            .await
            .unwrap_or_default()
    }

    async fn capabilities(&self, query: CapabilityQuery) -> Result<CapabilityReport, CaptureError> {
        let engine = self.engine();
        offload_open("capabilities", move || engine.capabilities(&query)).await
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

    fn watch_sources(
        &self,
        ctx: HostCtx,
        filter: SourceFilter,
    ) -> flume::Receiver<Result<SourceChange, CaptureError>> {
        // Bounded: a subscriber that stops reading holds only its own
        // watch thread.
        let (tx, rx) = flume::bounded(WATCH_DEPTH);
        if let Err(err) = self.admit(&ctx, "watch_sources") {
            let _queued = tx.try_send(Err(err));
            return rx;
        }
        let engine = self.engine();
        let failed = tx.clone();
        let spawned = std::thread::Builder::new()
            .name("istmo-screen-capture-watch".to_owned())
            .spawn(move || engine.watch_sources(filter, tx));
        if let Err(err) = spawned {
            tracing::warn!(%err, "screen-capture: a source watch could not start");
            let _queued = failed.try_send(Err(internal(
                "watch_sources",
                "the watch thread could not start",
            )));
        }
        rx
    }

    async fn access(&self, scope: AccessScope) -> Result<AccessState, CaptureError> {
        let engine = self.engine();
        #[cfg(all(windows, feature = "windows-common"))]
        if scope == AccessScope::BorderlessCapture {
            // WinRT capability queries: off the caller's executor.
            return offload_open("access", move || {
                Ok(windows::access::current(scope).unwrap_or_else(|| engine.access(scope)))
            })
            .await;
        }
        offload_open("access", move || Ok(engine.access(scope))).await
    }

    async fn request_access(
        &self,
        ctx: HostCtx,
        request: AccessRequest,
    ) -> Result<AccessState, CaptureError> {
        self.admit(&ctx, "request_access")?;
        let engine = self.engine();
        #[cfg(all(windows, feature = "windows-common"))]
        if request.scope == AccessScope::BorderlessCapture {
            // The OS prompt blocks until the person answers.
            return offload("request_access", move || {
                windows::access::request(request.scope, engine.policy())
                    .unwrap_or_else(|| Ok(engine.request_access(request.scope)))
            })
            .await;
        }
        // Only backends with a standing grant prompt (macOS TCC); the rest
        // answer `NotApplicable`.
        offload("request_access", move || {
            Ok(engine.request_access(request.scope))
        })
        .await
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
        offload_owned(
            "screenshot",
            move |caller| engine.screenshot_into(&request, &sender, origin, caller),
            drop,
        )
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
        offload_owned(
            "screenshot",
            move |caller| engine.screenshot_pixels(&request, layout, caller),
            drop,
        )
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
        let undo = undo_open(&self.engine, origin);
        offload_owned(
            "open",
            move |caller| engine.open(&runtime, &config, &video, None, origin, caller),
            undo,
        )
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
        let undo = undo_open(&self.engine, origin);
        offload_owned(
            "open",
            move |caller| engine.open(&runtime, &config, &video, Some(audio), origin, caller),
            undo,
        )
        .await
    }

    async fn open_audio(
        &self,
        ctx: HostCtx,
        config: AudioConfig,
        audio: FrameSinkId<AudioMeta>,
    ) -> Result<AudioCaptureSession, CaptureError> {
        self.admit(&ctx, "open_audio")?;
        let audio = ctx
            .frame_sender(audio)
            .map_err(|_| CaptureError::SinkUnreachable)?;
        let runtime = ctx.runtime().map_err(CaptureError::from)?;
        let engine = self.engine();
        let origin = ctx.origin();
        let undo = {
            let engine = Arc::clone(&engine);
            move |session: AudioCaptureSession| {
                let _closed = engine.close_audio(session.session, origin);
            }
        };
        offload_owned(
            "open_audio",
            move |caller| engine.open_audio(&runtime, &config, audio, origin, caller),
            undo,
        )
        .await
    }

    async fn update_audio(
        &self,
        ctx: HostCtx,
        session: NativeHandleId,
        update: AudioUpdate,
    ) -> Result<AudioNegotiated, CaptureError> {
        self.admit(&ctx, "update_audio")?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("update_audio", move || {
            engine.update_audio(session, update, origin)
        })
        .await
    }

    async fn close_audio(&self, ctx: HostCtx, session: NativeHandleId) -> Result<(), CaptureError> {
        self.admit(&ctx, "close_audio")?;
        let engine = self.engine();
        let origin = ctx.origin();
        offload("close_audio", move || engine.close_audio(session, origin)).await
    }

    fn audio_events(
        &self,
        ctx: HostCtx,
        session: NativeHandleId,
    ) -> flume::Receiver<Result<AudioEvent, CaptureError>> {
        if let Err(err) = self.admit(&ctx, "audio_events") {
            let (tx, rx) = flume::bounded(1);
            let _queued = tx.try_send(Err(err));
            return rx;
        }
        self.engine.audio_events(session, ctx.origin())
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
        let origin = ctx.origin();
        offload("close", move || engine.close(session, origin)).await
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
        self.engine.events(session, ctx.origin())
    }
}

/// Closes a session whose `open` caller stopped waiting after the
/// session was stored: nobody else knows its id.
fn undo_open(engine: &Arc<Engine>, origin: CallOrigin) -> impl FnOnce(CaptureSession) + use<> {
    let engine = Arc::clone(engine);
    move |session| {
        let _closed = engine.close(session.session, origin);
    }
}
