package dev.istmo.plugins.screencapture

import android.content.Context
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoFrameSender
import dev.istmo.runtime.NativeHandleId
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

/**
 * Reference backend for `istmo.screen_capture` on Android.
 *
 * MediaProjection capture is not implemented yet: every capture method
 * fails with `CaptureError.NoBackendCompiled` (and ends the sinks it was
 * handed with that reason), `backends()` reports the MediaProjection
 * backend as not compiled, and the reports say nothing is capturable.
 * Nothing here asks for consent, starts a service or holds a projection.
 */
@Suppress("UNUSED_PARAMETER")
class ScreenCaptureBackendImpl(context: Context) : ScreenCaptureBackend {

    private fun unsupported(): Nothing = throw BackendException(CaptureError.NoBackendCompiled)

    private fun <M> refuse(sink: IstmoFrameSender<M>): Nothing {
        sink.closeWithError(NOT_IMPLEMENTED)
        unsupported()
    }

    override suspend fun backends(): List<BackendStatus> = listOf(
        BackendStatus(BackendId.AndroidProjection, Availability.NotCompiled(NOT_IMPLEMENTED)),
    )

    override suspend fun capabilities(query: CapabilityQuery): CapabilityReport = unsupported()

    override suspend fun sources(filter: SourceFilter): SourceList = unsupported()

    override suspend fun access(scope: AccessScope): AccessState = unsupported()

    override suspend fun request_access(request: AccessRequest): AccessState = unsupported()

    override suspend fun request_projection(request: ProjectionRequest): ProjectionGrant = unsupported()

    override suspend fun screenshot(request: ScreenshotRequest, sink: IstmoFrameSender<FrameMeta>): ScreenshotInfo =
        refuse(sink)

    override suspend fun screenshot_pixels(request: ScreenshotRequest, layout: PixelLayout): RawImage = unsupported()

    override suspend fun open(config: SessionConfig, video: IstmoFrameSender<FrameMeta>): CaptureSession =
        refuse(video)

    override suspend fun open_with_audio(
        config: SessionConfig,
        video: IstmoFrameSender<FrameMeta>,
        audio: IstmoFrameSender<AudioMeta>,
    ): CaptureSession {
        audio.closeWithError(NOT_IMPLEMENTED)
        refuse(video)
    }

    override suspend fun open_preview(config: SessionConfig, target: PreviewTarget): CaptureSession = unsupported()

    override suspend fun open_preview_with_frames(
        config: SessionConfig,
        target: PreviewTarget,
        video: IstmoFrameSender<FrameMeta>,
    ): CaptureSession = refuse(video)

    override suspend fun update(session: NativeHandleId, update: SessionUpdate): NegotiatedStream =
        throw BackendException(CaptureError.SessionNotFound)

    override suspend fun close(session: NativeHandleId): Unit = throw BackendException(CaptureError.SessionNotFound)

    override fun events(session: NativeHandleId): Flow<CaptureEvent> = flow {
        throw BackendException(CaptureError.SessionNotFound)
    }

    private companion object {
        const val NOT_IMPLEMENTED = "MediaProjection capture is not implemented yet"
    }
}
