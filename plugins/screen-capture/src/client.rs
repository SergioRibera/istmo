//! Rust client sugar: sessions that bundle their sinks and events, and
//! screenshot helpers.

use std::sync::Arc;

use istmo_core::{FrameSink, NativeFrame, NativeHandle, Runtime, SinkPolicy, TypedStream};

use crate::ScreenCaptureClient;
use crate::types::{
    AudioMeta, CaptureError, CaptureEvent, CaptureSessionTag, FrameMeta, NegotiatedStream,
    PixelLayout, RawImage, ScreenshotInfo, ScreenshotRequest, SessionConfig, SessionUpdate,
};

/// A live session: the owned handle (dropping it closes the session),
/// its sinks and what was negotiated.
#[derive(Debug)]
pub struct SessionHandle {
    client: Arc<ScreenCaptureClient>,
    session: NativeHandle<CaptureSessionTag>,
    negotiated: NegotiatedStream,
    video: FrameSink<FrameMeta>,
    audio: Option<FrameSink<AudioMeta>>,
}

impl SessionHandle {
    #[must_use]
    pub const fn negotiated(&self) -> &NegotiatedStream {
        &self.negotiated
    }

    #[must_use]
    pub const fn id(&self) -> istmo_core::NativeHandleId {
        self.session.id()
    }

    /// The video frames.
    #[must_use]
    pub const fn video(&self) -> &FrameSink<FrameMeta> {
        &self.video
    }

    /// The audio chunks, for a session opened with audio.
    #[must_use]
    pub const fn audio(&self) -> Option<&FrameSink<AudioMeta>> {
        self.audio.as_ref()
    }

    /// The session's events.
    ///
    /// # Errors
    /// Transport failures, as [`CaptureError`].
    pub fn events(&self) -> Result<TypedStream<CaptureEvent, CaptureError>, CaptureError> {
        self.client
            .events(self.session.id())
            .map_err(CaptureError::from)
    }

    /// Renegotiate; on success the handle's `negotiated` is updated.
    ///
    /// # Errors
    /// The backend's refusal; the session stays as it was.
    pub async fn update(
        &mut self,
        update: SessionUpdate,
    ) -> Result<&NegotiatedStream, CaptureError> {
        let negotiated = self
            .client
            .update(self.session.id(), update)
            .await
            .map_err(CaptureError::from)?;
        self.negotiated = negotiated;
        Ok(&self.negotiated)
    }

    /// The next frame, taken from the video sink (the newest one for a
    /// `latest()` sink). `None` once the session ended and the sink is
    /// drained.
    #[must_use]
    pub fn grab_frame(&self) -> Option<NativeFrame<FrameMeta>> {
        self.video.recv()
    }

    /// Close explicitly and get the backend's answer.
    ///
    /// # Errors
    /// The backend's close error; the handle's drop still releases the
    /// session.
    pub async fn close(self) -> Result<(), CaptureError> {
        let id = self.session.id();
        self.client.close(id).await.map_err(CaptureError::from)?;
        let _closed: istmo_core::NativeHandleId = self.session.into_id();
        Ok(())
    }
}

/// Session and screenshot helpers on `Arc<ScreenCaptureClient>`.
pub trait ScreenCaptureClientExt {
    /// Open a video sink with `policy`, then a session feeding it.
    ///
    /// # Errors
    /// Sink creation or the backend's refusal.
    fn open_session(
        &self,
        runtime: &Arc<Runtime>,
        config: SessionConfig,
        policy: SinkPolicy,
    ) -> impl std::future::Future<Output = Result<SessionHandle, CaptureError>> + Send;

    /// As [`open_session`](Self::open_session), plus an audio sink
    /// (`queue(32)` with `DropNewest`, a contiguous prefix).
    ///
    /// # Errors
    /// Sink creation or the backend's refusal.
    fn open_session_with_audio(
        &self,
        runtime: &Arc<Runtime>,
        config: SessionConfig,
        policy: SinkPolicy,
    ) -> impl std::future::Future<Output = Result<SessionHandle, CaptureError>> + Send;

    /// One screenshot as frames: the sink holds `info.frames` frames.
    ///
    /// # Errors
    /// Sink creation or the backend's refusal.
    fn screenshot_frames(
        &self,
        runtime: &Arc<Runtime>,
        request: ScreenshotRequest,
    ) -> impl std::future::Future<
        Output = Result<(ScreenshotInfo, Vec<NativeFrame<FrameMeta>>), CaptureError>,
    > + Send;

    /// One screenshot as raw CPU pixels (never encoded).
    ///
    /// # Errors
    /// The backend's refusal.
    fn screenshot_cpu(
        &self,
        request: ScreenshotRequest,
        layout: PixelLayout,
    ) -> impl std::future::Future<Output = Result<RawImage, CaptureError>> + Send;
}

/// Frames a multi-monitor screenshot can return at most (one per
/// monitor); the sink must hold all of them.
const SCREENSHOT_SINK_DEPTH: usize = 16;

impl ScreenCaptureClientExt for Arc<ScreenCaptureClient> {
    async fn open_session(
        &self,
        runtime: &Arc<Runtime>,
        config: SessionConfig,
        policy: SinkPolicy,
    ) -> Result<SessionHandle, CaptureError> {
        let video = runtime
            .open_frame_sink::<FrameMeta>(policy)
            .map_err(CaptureError::from)?;
        let session = self
            .open_owned(config, video.id())
            .await
            .map_err(CaptureError::from)?;
        Ok(SessionHandle {
            client: Self::clone(self),
            session: session.session,
            negotiated: session.negotiated,
            video,
            audio: None,
        })
    }

    async fn open_session_with_audio(
        &self,
        runtime: &Arc<Runtime>,
        config: SessionConfig,
        policy: SinkPolicy,
    ) -> Result<SessionHandle, CaptureError> {
        let video = runtime
            .open_frame_sink::<FrameMeta>(policy)
            .map_err(CaptureError::from)?;
        let audio = runtime
            .open_frame_sink::<AudioMeta>(
                SinkPolicy::queue(32).with_overflow(istmo_core::Overflow::DropNewest),
            )
            .map_err(CaptureError::from)?;
        let session = self
            .open_with_audio_owned(config, video.id(), audio.id())
            .await
            .map_err(CaptureError::from)?;
        Ok(SessionHandle {
            client: Self::clone(self),
            session: session.session,
            negotiated: session.negotiated,
            video,
            audio: Some(audio),
        })
    }

    async fn screenshot_frames(
        &self,
        runtime: &Arc<Runtime>,
        request: ScreenshotRequest,
    ) -> Result<(ScreenshotInfo, Vec<NativeFrame<FrameMeta>>), CaptureError> {
        let sink = runtime
            .open_frame_sink::<FrameMeta>(SinkPolicy::queue(SCREENSHOT_SINK_DEPTH))
            .map_err(CaptureError::from)?;
        let info = self
            .screenshot(request, sink.id())
            .await
            .map_err(CaptureError::from)?;
        let frames = std::iter::from_fn(|| sink.try_recv()).collect();
        Ok((info, frames))
    }

    async fn screenshot_cpu(
        &self,
        request: ScreenshotRequest,
        layout: PixelLayout,
    ) -> Result<RawImage, CaptureError> {
        self.screenshot_pixels(request, layout)
            .await
            .map_err(CaptureError::from)
    }
}

#[cfg(test)]
impl SessionHandle {
    /// The video sink and the handle, separated (tests drop the sink
    /// while the session runs).
    pub(crate) fn into_parts_for_test(
        self,
    ) -> (FrameSink<FrameMeta>, NativeHandle<CaptureSessionTag>) {
        (self.video, self.session)
    }
}
