import Foundation
import IstmoRuntime

/// Reference backend for `istmo.screen_capture` on iOS / iPadOS.
///
/// ScreenCaptureKit (iOS 27) and ReplayKit capture are not implemented
/// yet: every capture method throws `ScreenCapture.CaptureError.noBackendCompiled`
/// (ending the sinks it was handed with that reason), and `backends()`
/// reports both backends as not compiled. Nothing here presents a
/// picker or starts a capture.
public final class ScreenCaptureBackendImpl: ScreenCaptureBackend {
    private static let notImplemented = "ScreenCaptureKit / ReplayKit capture is not implemented yet"

    public init() {}

    private func refuse<M>(_ sink: IstmoFrameSender<M>) -> ScreenCapture.CaptureError {
        sink.close(error: Self.notImplemented)
        return .noBackendCompiled
    }

    public func backends() async throws -> [ScreenCapture.BackendStatus] {
        [
            ScreenCapture.BackendStatus(backend: .iosSck, availability: .notCompiled(Self.notImplemented)),
            ScreenCapture.BackendStatus(backend: .iosReplayKit, availability: .notCompiled(Self.notImplemented)),
        ]
    }

    public func capabilities(query: ScreenCapture.CapabilityQuery) async throws -> ScreenCapture.CapabilityReport {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func sources(filter: ScreenCapture.SourceFilter) async throws -> ScreenCapture.SourceList {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func watch_sources(filter: ScreenCapture.SourceFilter) -> AsyncThrowingStream<ScreenCapture.SourceChange, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: ScreenCapture.CaptureError.noBackendCompiled)
        }
    }

    public func access(scope: ScreenCapture.AccessScope) async throws -> ScreenCapture.AccessState {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func request_access(request: ScreenCapture.AccessRequest) async throws -> ScreenCapture.AccessState {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func request_projection(request: ScreenCapture.ProjectionRequest) async throws -> ScreenCapture.ProjectionGrant {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func screenshot(request: ScreenCapture.ScreenshotRequest, sink: IstmoFrameSender<ScreenCapture.FrameMeta>) async throws -> ScreenCapture.ScreenshotInfo {
        throw refuse(sink)
    }

    public func screenshot_pixels(request: ScreenCapture.ScreenshotRequest, layout: ScreenCapture.PixelLayout) async throws -> ScreenCapture.RawImage {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func `open`(config: ScreenCapture.SessionConfig, video: IstmoFrameSender<ScreenCapture.FrameMeta>) async throws -> ScreenCapture.CaptureSession {
        throw refuse(video)
    }

    public func open_with_audio(
        config: ScreenCapture.SessionConfig,
        video: IstmoFrameSender<ScreenCapture.FrameMeta>,
        audio: IstmoFrameSender<ScreenCapture.AudioMeta>
    ) async throws -> ScreenCapture.CaptureSession {
        audio.close(error: Self.notImplemented)
        throw refuse(video)
    }

    public func open_audio(config: ScreenCapture.AudioConfig, audio: IstmoFrameSender<ScreenCapture.AudioMeta>) async throws -> ScreenCapture.AudioCaptureSession {
        throw refuse(audio)
    }

    public func update_audio(session: NativeHandleId, update: ScreenCapture.AudioUpdate) async throws -> ScreenCapture.AudioNegotiated {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func close_audio(session: NativeHandleId) async throws {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func audio_events(session: NativeHandleId) -> AsyncThrowingStream<ScreenCapture.AudioEvent, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: ScreenCapture.CaptureError.noBackendCompiled)
        }
    }

    public func open_preview(config: ScreenCapture.SessionConfig, target: ScreenCapture.PreviewTarget) async throws -> ScreenCapture.CaptureSession {
        throw ScreenCapture.CaptureError.noBackendCompiled
    }

    public func open_preview_with_frames(
        config: ScreenCapture.SessionConfig,
        target: ScreenCapture.PreviewTarget,
        video: IstmoFrameSender<ScreenCapture.FrameMeta>
    ) async throws -> ScreenCapture.CaptureSession {
        throw refuse(video)
    }

    public func update(session: NativeHandleId, update: ScreenCapture.SessionUpdate) async throws -> ScreenCapture.NegotiatedStream {
        throw ScreenCapture.CaptureError.sessionNotFound
    }

    public func close(session: NativeHandleId) async throws {
        throw ScreenCapture.CaptureError.sessionNotFound
    }

    public func events(session: NativeHandleId) -> AsyncThrowingStream<ScreenCapture.CaptureEvent, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: ScreenCapture.CaptureError.sessionNotFound)
        }
    }
}
