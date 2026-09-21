#if canImport(UIKit)
import UIKit

/// Reference `PenBackend` implementation for iOS / iPadOS.
///
/// One instance per window / drawing surface. The [`PenCaptureView`]
/// provides the actual stylus event source; this wrapper surfaces its
/// `AsyncThrowingStream`s through the plugin interface.
///
/// `windowId` is echoed from `PenConfig`; consumers can log or route
/// by it but the mapping from id to view is the factory's job.
public final class PenBackendImpl: PenBackend {

    private let view: PenCaptureView
    // Echoed for diagnostics; the factory decides which view each
    // window id maps to. Suppressed via a no-op read in `init`.
    private let windowId: UInt64

    public init(view: PenCaptureView, windowId: UInt64) {
        self.view = view
        self.windowId = windowId
        _ = self.windowId
    }

    public func events() -> AsyncThrowingStream<PenEvent, Error> {
        view.events
    }

    public func hover() -> AsyncThrowingStream<PenHoverEvent, Error> {
        view.hover
    }

    public func capabilities() async throws -> PenCapabilities {
        view.capabilities
    }

    public func set_prediction_enabled(enabled: Bool) async throws {
        view.setPredictionEnabled(enabled)
    }
}

/// Reference `PenFactory` for single-window apps.
///
/// Multi-window apps need a per-window `PenCaptureView` and cannot use
/// this factory directly — write a bespoke factory that maps
/// `PenConfig.windowId` to the matching view. Because the factory
/// requires a `PenCaptureView` reference the app assembles at
/// UI-construction time, `istmo-pen` opts out of auto-registration
/// (`auto_register = false` in its `istmo.toml`) and expects the app
/// to register the host manually:
///
/// ```swift
/// let view = PenCaptureView(frame: .zero)
/// IstmoRuntime.shared.registerHandler(PenFactoryImpl(view: view))
/// ```
public final class PenFactoryImpl: PenFactory {

    private let view: PenCaptureView

    public init(view: PenCaptureView) {
        self.view = view
    }

    public func create(config: PenConfig) async throws -> PenBackend {
        PenBackendImpl(view: view, windowId: config.windowId)
    }
}

#endif
