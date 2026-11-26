import Foundation
import IstmoRuntime

/// Reference backend for `istmo.screen_capture` on iOS / iPadOS.
///
/// ScreenCaptureKit (iOS 27) and ReplayKit capture are not implemented
/// yet: every capture method throws `CaptureError.noBackendCompiled`
/// (ending the sinks it was handed with that reason), and `backends()`
/// reports both backends as not compiled. Nothing here presents a
/// picker or starts a capture.
public final class ScreenCaptureBackendImpl: ScreenCaptureBackend {
    private static let notImplemented = "ScreenCaptureKit / ReplayKit capture is not implemented yet"

    public init() {}

    private func refuse<M>(_ sink: IstmoFrameSender<M>) -> CaptureError {
        sink.close(error: Self.notImplemented)
        return .noBackendCompiled
    }

    public func backends() async throws -> [BackendStatus] {
        [
            BackendStatus(backend: .iosSck, availability: .notCompiled(Self.notImplemented)),
            BackendStatus(backend: .iosReplayKit, availability: .notCompiled(Self.notImplemented)),
        ]
    }

    public func capabilities(query: CapabilityQuery) async throws -> CapabilityReport {
        throw CaptureError.noBackendCompiled
    }

    public func sources(filter: SourceFilter) async throws -> SourceList {
        throw CaptureError.noBackendCompiled
    }

    public func access(scope: AccessScope) async throws -> AccessState {
        throw CaptureError.noBackendCompiled
    }

    public func request_access(request: AccessRequest) async throws -> AccessState {
        throw CaptureError.noBackendCompiled
    }

    public func request_projection(request: ProjectionRequest) async throws -> ProjectionGrant {
        throw CaptureError.noBackendCompiled
    }

    public func screenshot(request: ScreenshotRequest, sink: IstmoFrameSender<FrameMeta>) async throws -> ScreenshotInfo {
        throw refuse(sink)
    }

    public func screenshot_pixels(request: ScreenshotRequest, layout: PixelLayout) async throws -> RawImage {
        throw CaptureError.noBackendCompiled
    }

    public func `open`(config: SessionConfig, video: IstmoFrameSender<FrameMeta>) async throws -> CaptureSession {
        throw refuse(video)
    }

    public func open_with_audio(
        config: SessionConfig,
        video: IstmoFrameSender<FrameMeta>,
        audio: IstmoFrameSender<AudioMeta>
    ) async throws -> CaptureSession {
        audio.close(error: Self.notImplemented)
        throw refuse(video)
    }

    public func open_preview(config: SessionConfig, target: PreviewTarget) async throws -> CaptureSession {
        throw CaptureError.noBackendCompiled
    }

    public func open_preview_with_frames(
        config: SessionConfig,
        target: PreviewTarget,
        video: IstmoFrameSender<FrameMeta>
    ) async throws -> CaptureSession {
        throw refuse(video)
    }

    public func update(session: NativeHandleId, update: SessionUpdate) async throws -> NegotiatedStream {
        throw CaptureError.sessionNotFound
    }

    public func close(session: NativeHandleId) async throws {
        throw CaptureError.sessionNotFound
    }

    public func events(session: NativeHandleId) -> AsyncThrowingStream<CaptureEvent, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: CaptureError.sessionNotFound)
        }
    }
}
