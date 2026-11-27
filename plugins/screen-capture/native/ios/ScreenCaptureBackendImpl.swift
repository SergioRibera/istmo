import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import IOSurface
import IstmoRuntime
import ReplayKit

/// `istmo.screen_capture` on iOS / iPadOS.
///
/// Two backends, both picker-only (nothing is listed; the person chooses
/// in system UI, so `ConsentConfig.allowPicker` must be set):
///
/// - `iosSck` (iOS / iPadOS 27): ScreenCaptureKit in-process. The shared
///   `SCContentSharingPicker` gives a content filter (the full display, or
///   the app's own content); frames are the stream's `IOSurface`-backed
///   `CVPixelBuffer`s, handed to Rust retained (no copy), with damage,
///   content rect, scale and `videoOrientation` from the frame info.
/// - `iosReplayKit` (before 27, deprecated there): `RPScreenRecorder`
///   in-app capture after the system alert, foreground only
///   (`paused(.backgrounded)` while in the background), the system's pixel
///   format and pool.
///
/// Both stamp frames on the host clock (`ptsClock = .hostTime`) and give
/// `wallUs` per frame. Neither joins `Auto` before its device checklist
/// passed: name them with `.fixed` / `.ordered`. The broadcast-upload
/// extension (system-wide capture before 27, `RPSystemBroadcastPickerView`)
/// is not built.
///
/// Previews: `open_preview` enqueues the sample buffers into the app's
/// `AVSampleBufferDisplayLayer` (registered with
/// `IstmoRuntime.shared.registerObject(layer)`), retained, no copy;
/// `open_preview_with_frames` also hands the same buffers to the sink.
public final class ScreenCaptureBackendImpl: ScreenCaptureBackend, HandleReleaser, @unchecked Sendable {
    static let pluginId = "istmo.screen_capture"
    /// A backend joins `Auto` once its device checklist passed.
    static let joinsAuto: Set<ScreenCapture.BackendId> = []

    private let lock = NSLock()
    private var sessions: [NativeHandleId: ScreenCaptureLiveSession] = [:]
    private var audioSessions: [NativeHandleId: ScreenCaptureAudioSession] = [:]

    public init() {}

    // MARK: Backends and capabilities

    private static var sckRuns: Bool {
        #if canImport(ScreenCaptureKit)
        if #available(iOS 27.0, *) { return true }
        #endif
        return false
    }

    private static func availability(_ id: ScreenCapture.BackendId) -> ScreenCapture.Availability {
        switch id {
        case .iosSck:
            #if canImport(ScreenCaptureKit)
            return sckRuns ? .ready : .osTooOld(ScreenCapture.OsVersion(major: 27, minor: 0, patch: 0))
            #else
            return .notCompiled("this SDK has no iOS ScreenCaptureKit")
            #endif
        case .iosReplayKit:
            return RPScreenRecorder.shared().isAvailable
                ? .ready : .notOnThisSession("ReplayKit screen recording is unavailable")
        default:
            return .notCompiled("not an iOS backend")
        }
    }

    private static func isReady(_ availability: ScreenCapture.Availability) -> Bool {
        if case .ready = availability { return true }
        return false
    }

    public func backends() async throws -> [ScreenCapture.BackendStatus] {
        [ScreenCapture.BackendId.iosSck, .iosReplayKit].map {
            ScreenCapture.BackendStatus(backend: $0, availability: Self.availability($0))
        }
    }

    public func capabilities(query: ScreenCapture.CapabilityQuery) async throws -> ScreenCapture.CapabilityReport {
        let wanted = query.backend ?? query.source?.backend
        if let wanted, wanted != .iosSck, wanted != .iosReplayKit {
            throw ScreenCapture.CaptureError.backendNotCompiled(wanted)
        }
        var reports: [ScreenCapture.BackendCaps] = []
        for id in [ScreenCapture.BackendId.iosSck, .iosReplayKit] where wanted == nil || wanted == id {
            let availability = Self.availability(id)
            guard Self.isReady(availability) else {
                if wanted != nil {
                    throw ScreenCapture.CaptureError.backendUnavailable(
                        ScreenCapture.BackendReason(backend: id, availability: availability))
                }
                continue
            }
            let caps = ScreenCaptureTables.caps(id)
            if let kind = query.kind, !caps.sourceKinds.contains(kind) { continue }
            reports.append(caps)
        }
        let audioBackend: ScreenCapture.BackendId = Self.sckRuns ? .iosSck : .iosReplayKit
        let audio = ScreenCapture.AudioReport(
            availability: Self.availability(audioBackend), caps: ScreenCaptureTables.audioCaps(audioBackend))
        return ScreenCapture.CapabilityReport(backends: reports, audio: audio)
    }

    public func sources(filter: ScreenCapture.SourceFilter) async throws -> ScreenCapture.SourceList {
        // Nothing is listed on iOS: the person picks in system UI.
        ScreenCapture.SourceList(sources: [], listing: .pickerOnly)
    }

    public func watch_sources(filter: ScreenCapture.SourceFilter) -> AsyncThrowingStream<ScreenCapture.SourceChange, Error> {
        AsyncThrowingStream { continuation in
            continuation.finish(throwing: Self.unmet(nil, .operation))
        }
    }

    public func access(scope: ScreenCapture.AccessScope) async throws -> ScreenCapture.AccessState {
        Self.accessState(scope)
    }

    public func request_access(request: ScreenCapture.AccessRequest) async throws -> ScreenCapture.AccessState {
        // No standing grant on iOS: consent is per session (picker / alert).
        Self.accessState(request.scope)
    }

    private static func accessState(_ scope: ScreenCapture.AccessScope) -> ScreenCapture.AccessState {
        guard scope == .screenRecording else {
            return ScreenCapture.AccessState(scope: scope, status: .notApplicable, model: .noneNeeded,
                                             persistence: .unsupported, revocableMidSession: false,
                                             indicator: .notApplicable)
        }
        return ScreenCapture.AccessState(
            scope: scope, status: .perSession, model: sckRuns ? .pickerPerSession : .perSessionSystemAlert,
            persistence: .unsupported, revocableMidSession: true, indicator: .shown)
    }

    public func request_projection(request: ScreenCapture.ProjectionRequest) async throws -> ScreenCapture.ProjectionGrant {
        throw Self.unmet(nil, .operation)
    }

    // MARK: Selection

    static func unmet(_ backend: ScreenCapture.BackendId?, _ requirement: ScreenCapture.Requirement) -> ScreenCapture.CaptureError {
        .unsatisfiable(ScreenCapture.Unmet(backend: backend, requirements: [requirement]))
    }

    /// The backend `selection` names, ready to run.
    private static func choose(_ selection: ScreenCapture.BackendSelection) throws -> ScreenCapture.BackendId {
        let ours: [ScreenCapture.BackendId] = [.iosSck, .iosReplayKit]
        func ready(_ id: ScreenCapture.BackendId) throws -> ScreenCapture.BackendId {
            guard ours.contains(id) else { throw ScreenCapture.CaptureError.backendNotCompiled(id) }
            let availability = availability(id)
            guard isReady(availability) else {
                throw ScreenCapture.CaptureError.backendUnavailable(
                    ScreenCapture.BackendReason(backend: id, availability: availability))
            }
            return id
        }
        switch selection {
        case let .fixed(id):
            return try ready(id)
        case let .ordered(ids):
            var attempts: [ScreenCapture.BackendAttempt] = []
            for id in ids {
                if let id = try? ready(id) { return id }
                attempts.append(ScreenCapture.BackendAttempt(backend: id, failure: .unavailable(availability(id))))
            }
            throw ScreenCapture.CaptureError.noBackend(attempts)
        case let .auto(auto):
            // iOS 27+: ScreenCaptureKit; before: ReplayKit in-app.
            let order: [ScreenCapture.BackendId] = sckRuns ? [.iosSck] : [.iosReplayKit]
            var attempts: [ScreenCapture.BackendAttempt] = []
            for id in order where !auto.exclude.contains(id) {
                guard joinsAuto.contains(id) else {
                    attempts.append(ScreenCapture.BackendAttempt(backend: id, failure: .unavailable(.notOnThisSession(
                        "not verified on a device yet; name it with .fixed or .ordered"))))
                    continue
                }
                if isReady(availability(id)) { return id }
                attempts.append(ScreenCapture.BackendAttempt(backend: id, failure: .unavailable(availability(id))))
            }
            throw ScreenCapture.CaptureError.noBackend(attempts)
        }
    }

    /// The backend audio-only sessions run on. `AudioConfig` names no
    /// backend, and neither backend joins `Auto` yet, so this is the one
    /// the OS has (ScreenCaptureKit on 27, ReplayKit before), not `Auto`.
    static func audioBackend() throws -> ScreenCapture.BackendId {
        let id: ScreenCapture.BackendId = sckRuns ? .iosSck : .iosReplayKit
        let availability = availability(id)
        guard isReady(availability) else {
            throw ScreenCapture.CaptureError.backendUnavailable(
                ScreenCapture.BackendReason(backend: id, availability: availability))
        }
        return id
    }

    /// Whether `selector` asks for this app's own content only (a picker
    /// offering applications and nothing else).
    static func appOnly(_ selector: ScreenCapture.SourceSelector) -> Bool {
        if case let .picker(options) = selector { return options.kinds == [.application] }
        return false
    }

    /// Whether a picker can honour `selector`, under `consent`.
    private static func checkSource(_ selector: ScreenCapture.SourceSelector, consent: ScreenCapture.ConsentConfig,
                                    backend: ScreenCapture.BackendId) throws {
        switch selector {
        case .picker, .primaryMonitor:
            break
        case .id:
            guard consent.allowPicker else { throw unmet(backend, .windowById) }
        case let .monitors(ids) where ids.count == 1:
            guard consent.allowPicker else { throw unmet(backend, .windowById) }
        case .allMonitors, .monitors:
            throw unmet(backend, .multiMonitor)
        case .region:
            throw unmet(backend, .region)
        case .focused, .underCursor:
            throw unmet(backend, .sourceSelector)
        }
        guard consent.allowPicker else { throw ScreenCapture.CaptureError.consentRequired }
    }

    /// The tier, format and memory `request` gets on `backend`.
    private static func negotiate(_ request: ScreenCapture.FormatRequest, backend: ScreenCapture.BackendId) throws
        -> ScreenCaptureChoice {
        let offers = ScreenCaptureTables.caps(backend).video
        guard let choice = ScreenCaptureChoice.negotiate(request, offers: offers) else {
            throw unmet(backend, .formats)
        }
        return choice
    }

    /// The video requirements iOS cannot meet.
    private static func checkVideo(_ video: ScreenCapture.VideoConfig, backend: ScreenCapture.BackendId) throws {
        var unmet: [ScreenCapture.Requirement] = []
        if video.cursor.strength == .require, video.cursor.mode != .hidden { unmet.append(.cursor) }
        if video.damage == .require, backend == .iosReplayKit { unmet.append(.damage) }
        if video.hdr == .requireHdr { unmet.append(.hdr) }
        // Frames are never turned on the CPU here.
        if video.orientation == .upright { unmet.append(.upright) }
        if backend == .iosReplayKit {
            switch video.size {
            case .native: break
            case .scaled, .fit: unmet.append(.scaling)
            }
        }
        if !unmet.isEmpty {
            throw ScreenCapture.CaptureError.unsatisfiable(ScreenCapture.Unmet(backend: backend, requirements: unmet))
        }
    }

    private static func capturer(_ backend: ScreenCapture.BackendId) throws -> ScreenCapturer {
        #if canImport(ScreenCaptureKit)
        if backend == .iosSck, #available(iOS 27.0, *) { return SckCapturer() }
        #endif
        if backend == .iosReplayKit { return ReplayKitCapturer() }
        throw ScreenCapture.CaptureError.backendNotCompiled(backend)
    }

    private static func poolDepth(_ requested: UInt8) -> UInt8 {
        let pool = ScreenCaptureTables.pool
        return min(max(requested == 0 ? pool.defaultDepth : requested, pool.minDepth), pool.maxDepth)
    }

    // MARK: Screenshots

    /// One frame, through a capture started and stopped for it.
    private func shoot(_ request: ScreenCapture.ScreenshotRequest, backend: ScreenCapture.BackendId,
                       pixelFormat: OSType?) async throws -> ScreenCaptureShot {
        try Self.checkSource(request.source, consent: request.consent, backend: backend)
        if request.hdr == .requireHdr { throw Self.unmet(backend, .hdr) }
        let capturer = try Self.capturer(backend)
        let feed = ScreenCaptureOneShot(backend: backend)
        let source: ScreenCapture.Source
        do {
            source = try await capturer.start(
                ScreenCaptureRequest(pixelFormat: pixelFormat, fps: .native, size: .native,
                                     queueDepth: Int(ScreenCaptureTables.pool.minDepth), video: true, audio: nil,
                                     appOnly: Self.appOnly(request.source)),
                feed: feed)
        } catch {
            // Whatever part of the capture started stops with it.
            await capturer.stop()
            throw error
        }
        let shot = await feed.wait(timeoutNs: 10_000_000_000)
        await capturer.stop()
        let (sample, info) = try shot.get()
        guard let pixels = ScreenCapturePixels(sample) else {
            throw ScreenCaptureErrors.detail(backend, "screenshot", "no pixel buffer")
        }
        return ScreenCaptureShot(sample: sample, info: info, pixels: pixels, source: source)
    }

    public func screenshot(request: ScreenCapture.ScreenshotRequest, sink: IstmoFrameSender<ScreenCapture.FrameMeta>) async throws -> ScreenCapture.ScreenshotInfo {
        do {
            let backend = try Self.choose(request.backend)
            let chosen = try Self.negotiate(request.formats, backend: backend)
            let shot = try await shoot(
                request, backend: backend,
                pixelFormat: backend == .iosSck ? ScreenCaptureFourcc.code(for: chosen.format) : nil)
            guard let (format, range) = ScreenCaptureFourcc.format(shot.pixels.code) else {
                throw ScreenCaptureErrors.detail(backend, "screenshot", "a frame in an unknown pixel format")
            }
            let shape = ScreenCaptureShape(format: format, memory: chosen.memory,
                                           colour: ScreenCaptureFourcc.colour(format, range),
                                           tier: chosen.tier, sourceId: shot.source.id)
            let meta = shape.meta(sample: shot.sample, pixels: shot.pixels, info: shot.info, seq: 0,
                                  buffer: ScreenCapture.BufferIdentity(poolGeneration: 0, slot: 0), skipped: 0)
            let status: IstmoSubmitStatus
            if chosen.memory == .ioSurface, let surface = shot.pixels.surface {
                status = sink.submit(surface, meta: meta)
            } else {
                status = sink.submit(shot.pixels.buffer, meta: meta)
            }
            guard status.isTaken else {
                throw status == .full ? ScreenCapture.CaptureError.sinkFull : ScreenCapture.CaptureError.sinkUnreachable
            }
            let negotiated = ScreenCaptureTables.negotiated(
                backend: backend, source: shot.source, size: shot.pixels.size, shape: shape,
                fps: .native, poolDepth: 1, exhaustion: .stall)
            return ScreenCapture.ScreenshotInfo(frames: 1, backend: backend, negotiated: negotiated)
        } catch {
            sink.close(error: String(describing: error))
            throw error
        }
    }

    public func screenshot_pixels(request: ScreenCapture.ScreenshotRequest, layout: ScreenCapture.PixelLayout) async throws -> ScreenCapture.RawImage {
        let backend = try Self.choose(request.backend)
        let shot = try await shoot(request, backend: backend, pixelFormat: ScreenCaptureFourcc.bgra)
        let crop = shot.info.contentRect(in: shot.pixels.size)
        guard let raw = shot.pixels.raw(crop: crop, orientation: shot.info.pluginOrientation, layout: layout) else {
            throw Self.unmet(backend, .formats)
        }
        return ScreenCapture.RawImage(
            width: raw.width, height: raw.height, stride: raw.width * 4, format: layout.format,
            colour: ScreenCapture.Colour(space: .srgb, transfer: .srgb, range: .full),
            source: shot.source.id, bytes: Data(raw.bytes))
    }

    // MARK: Live sessions

    private func start(config: ScreenCapture.SessionConfig, video: IstmoFrameSender<ScreenCapture.FrameMeta>?,
                       audio: IstmoFrameSender<ScreenCapture.AudioMeta>?,
                       preview: (layer: AVSampleBufferDisplayLayer, fit: ScreenCapture.PreviewFit)?)
        async throws -> ScreenCapture.CaptureSession {
        let id = IstmoRuntime.shared.allocHandleId(pluginId: Self.pluginId)
        do {
            let backend = try Self.choose(config.backend)
            try Self.checkSource(config.source, consent: config.consent, backend: backend)
            try Self.checkVideo(config.video, backend: backend)
            if (config.audio != nil) != (audio != nil) { throw Self.unmet(backend, .audio) }
            let chosen = try Self.negotiate(config.video.formats, backend: backend)
            let audioRequest = try config.audio.map { try Self.audioRequest($0, backend: backend) }
            let depth = Self.poolDepth(config.video.poolDepth)
            // `exact` repeats are not made on iOS: it is enforced as `max`.
            let fps = ScreenCaptureChoice.enforced(config.video.fps)
            let request = ScreenCaptureRequest(
                pixelFormat: backend == .iosSck ? ScreenCaptureFourcc.code(for: chosen.format) : nil,
                fps: fps, size: config.video.size, queueDepth: Int(depth),
                video: video != nil || preview != nil, audio: audioRequest,
                appOnly: Self.appOnly(config.source))
            let capturer = try Self.capturer(backend)
            let audioOut = audio.map { sender in
                ScreenCaptureAudioOut(sender: sender, negotiated: ScreenCapture.AudioNegotiated(
                    source: config.audio?.source ?? .system, rate: audioRequest?.rate ?? 48_000,
                    channels: audioRequest?.channels ?? 2, format: config.audio?.format ?? .f32))
            }
            let session = ScreenCaptureLiveSession(
                backend: backend, capturer: capturer, request: request, video: video, audio: audioOut,
                layer: preview?.layer, chosen: chosen, exhaustion: config.video.exhaustion, depth: depth)
            if let preview {
                let gravity = ScreenCaptureTables.gravity(preview.fit)
                DispatchQueue.main.async { preview.layer.videoGravity = gravity }
            }
            let source = try await capturer.start(request, feed: session)
            let negotiated = session.begin(source: source, audio: audioOut?.negotiated)
            lock.withLock { sessions[id] = session }
            return ScreenCapture.CaptureSession(session: id, negotiated: negotiated)
        } catch {
            IstmoRuntime.shared.forgetHandle(id)
            let reason = String(describing: error)
            video?.close(error: reason)
            audio?.close(error: reason)
            throw error
        }
    }

    private static func audioRequest(_ config: ScreenCapture.AudioConfig, backend: ScreenCapture.BackendId) throws -> ScreenCaptureAudioRequest {
        let channels = config.channels ?? 2
        guard (1...2).contains(channels) else { throw unmet(backend, .audio) }
        let excludeSelf: Bool
        switch (config.source, backend) {
        // ScreenCaptureKit: what the picked content plays.
        case (.system, .iosSck): excludeSelf = false
        case (.excludeSelf, .iosSck): excludeSelf = true
        // ReplayKit in-app: this app's own audio only.
        case (.application, .iosReplayKit): excludeSelf = false
        default: throw unmet(backend, .audio)
        }
        return ScreenCaptureAudioRequest(rate: config.rate ?? 48_000, channels: channels, excludeSelf: excludeSelf)
    }

    public func `open`(config: ScreenCapture.SessionConfig, video: IstmoFrameSender<ScreenCapture.FrameMeta>) async throws -> ScreenCapture.CaptureSession {
        try await start(config: config, video: video, audio: nil, preview: nil)
    }

    public func open_with_audio(
        config: ScreenCapture.SessionConfig,
        video: IstmoFrameSender<ScreenCapture.FrameMeta>,
        audio: IstmoFrameSender<ScreenCapture.AudioMeta>
    ) async throws -> ScreenCapture.CaptureSession {
        try await start(config: config, video: video, audio: audio, preview: nil)
    }

    private static func layer(_ target: ScreenCapture.PreviewTarget) throws -> AVSampleBufferDisplayLayer {
        guard target.kind == .avSampleBufferDisplayLayer else { throw unmet(nil, .operation) }
        guard let layer = IstmoRuntime.shared.object(for: target.surface, as: AVSampleBufferDisplayLayer.self) else {
            throw ScreenCapture.CaptureError.sinkUnreachable
        }
        return layer
    }

    public func open_preview(config: ScreenCapture.SessionConfig, target: ScreenCapture.PreviewTarget) async throws -> ScreenCapture.CaptureSession {
        try await start(config: config, video: nil, audio: nil, preview: (Self.layer(target), target.fit))
    }

    public func open_preview_with_frames(
        config: ScreenCapture.SessionConfig,
        target: ScreenCapture.PreviewTarget,
        video: IstmoFrameSender<ScreenCapture.FrameMeta>
    ) async throws -> ScreenCapture.CaptureSession {
        let layer: AVSampleBufferDisplayLayer
        do { layer = try Self.layer(target) } catch {
            video.close(error: String(describing: error))
            throw error
        }
        // The same sample buffers go to the layer and to the sink: no copy.
        return try await start(config: config, video: video, audio: nil, preview: (layer, target.fit))
    }

    private func session(_ id: NativeHandleId) throws -> ScreenCaptureLiveSession {
        guard let session = lock.withLock({ sessions[id] }) else { throw ScreenCapture.CaptureError.sessionNotFound }
        return session
    }

    public func update(session: NativeHandleId, update: ScreenCapture.SessionUpdate) async throws -> ScreenCapture.NegotiatedStream {
        try await self.session(session).update(update)
    }

    public func close(session: NativeHandleId) async throws {
        guard let state = lock.withLock({ sessions.removeValue(forKey: session) }) else {
            throw ScreenCapture.CaptureError.sessionNotFound
        }
        await state.shutdown(.closed)
        IstmoRuntime.shared.forgetHandle(session)
    }

    public func events(session: NativeHandleId) -> AsyncThrowingStream<ScreenCapture.CaptureEvent, Error> {
        guard let state = lock.withLock({ sessions[session] }) else {
            return AsyncThrowingStream { $0.finish(throwing: ScreenCapture.CaptureError.sessionNotFound) }
        }
        return state.subscribe()
    }

    // MARK: Audio-only sessions

    public func open_audio(config: ScreenCapture.AudioConfig, audio: IstmoFrameSender<ScreenCapture.AudioMeta>) async throws -> ScreenCapture.AudioCaptureSession {
        do {
            let backend = try Self.audioBackend()
            let request = try Self.audioRequest(config, backend: backend)
            let capturer = try Self.capturer(backend)
            let negotiated = ScreenCapture.AudioNegotiated(source: config.source, rate: request.rate,
                                                          channels: request.channels, format: config.format)
            let out = ScreenCaptureAudioOut(sender: audio, negotiated: negotiated)
            let session = ScreenCaptureAudioSession(capturer: capturer, out: out)
            _ = try await capturer.start(
                ScreenCaptureRequest(pixelFormat: nil, fps: .max(1), size: .native,
                                     queueDepth: Int(ScreenCaptureTables.pool.minDepth), video: false, audio: request),
                feed: session)
            let id = IstmoRuntime.shared.allocHandleId(pluginId: Self.pluginId)
            out.events.emit(.started(negotiated))
            lock.withLock { audioSessions[id] = session }
            return ScreenCapture.AudioCaptureSession(session: id, negotiated: negotiated)
        } catch {
            audio.close(error: String(describing: error))
            throw error
        }
    }

    public func update_audio(session: NativeHandleId, update: ScreenCapture.AudioUpdate) async throws -> ScreenCapture.AudioNegotiated {
        guard let state = lock.withLock({ audioSessions[session] }) else { throw ScreenCapture.CaptureError.sessionNotFound }
        return state.out.apply(update)
    }

    public func close_audio(session: NativeHandleId) async throws {
        guard let state = lock.withLock({ audioSessions.removeValue(forKey: session) }) else {
            throw ScreenCapture.CaptureError.sessionNotFound
        }
        await state.capturer.stop()
        state.out.finish(.closed, error: nil)
        IstmoRuntime.shared.forgetHandle(session)
    }

    public func audio_events(session: NativeHandleId) -> AsyncThrowingStream<ScreenCapture.AudioEvent, Error> {
        guard let state = lock.withLock({ audioSessions[session] }) else {
            return AsyncThrowingStream { $0.finish(throwing: ScreenCapture.CaptureError.sessionNotFound) }
        }
        return state.out.events.subscribe(first: nil)
    }

    // MARK: Handles

    public func releaseNativeHandle(_ handleId: UInt64) {
        // Every release in the runtime reaches every releaser; ids this
        // backend never handed out are in neither table.
        let (live, audio) = lock.withLock {
            (sessions.removeValue(forKey: handleId), audioSessions.removeValue(forKey: handleId))
        }
        guard live != nil || audio != nil else { return }
        Task {
            await live?.shutdown(.closed)
            if let audio {
                await audio.capturer.stop()
                audio.out.finish(.closed, error: nil)
            }
            IstmoRuntime.shared.forgetHandle(handleId)
        }
    }
}

/// A frame taken for a screenshot.
struct ScreenCaptureShot {
    let sample: CMSampleBuffer
    let info: ScreenCaptureFrameInfo
    let pixels: ScreenCapturePixels
    let source: ScreenCapture.Source
}

/// What a format request gets.
struct ScreenCaptureChoice {
    var tier: UInt8
    var format: ScreenCapture.PixelFormat
    var memory: ScreenCapture.BufferMemory

    /// The first tier some offer satisfies (an empty request takes the
    /// first offer). No offer here makes a copy, so tiers' allowed copies
    /// never matter.
    static func negotiate(_ request: ScreenCapture.FormatRequest, offers: [ScreenCapture.FormatCaps]) -> ScreenCaptureChoice? {
        guard !request.tiers.isEmpty else {
            return offers.first.map { ScreenCaptureChoice(tier: 0, format: $0.format, memory: $0.memory) }
        }
        for (index, tier) in request.tiers.enumerated() {
            for choice in tier.choices where offers.contains(where: { $0.format == choice.format && $0.memory == choice.memory }) {
                return ScreenCaptureChoice(tier: UInt8(clamping: index), format: choice.format, memory: choice.memory)
            }
        }
        return nil
    }

    /// The rate as enforced: `exact` repeats are not made on iOS.
    static func enforced(_ fps: ScreenCapture.FrameRate) -> ScreenCapture.FrameRate {
        if case let .exact(n) = fps { return .max(n) }
        return fps
    }
}

/// Fixed facts per backend (the platform tables; nothing is probed).
enum ScreenCaptureTables {
    static let pool = ScreenCapture.PoolCaps(minDepth: 3, maxDepth: 8, defaultDepth: 4,
                                             exhaustion: [.stall, .dropNewest, .copyToCpu])

    static func audioCaps(_ backend: ScreenCapture.BackendId) -> ScreenCapture.AudioCaps {
        if backend == .iosSck {
            return ScreenCapture.AudioCaps(sources: [.system, .excludeSelf], formats: [.f32, .s16], mayBeSilent: false)
        }
        return ScreenCapture.AudioCaps(sources: [.application], formats: [.f32, .s16], mayBeSilent: false)
    }

    static func caps(_ backend: ScreenCapture.BackendId) -> ScreenCapture.BackendCaps {
        let sck = backend == .iosSck
        let formats: [ScreenCapture.PixelFormat] = sck ? [.bgra8, .nv12] : [.nv12, .bgra8]
        let video = formats.flatMap { format in
            [ScreenCapture.BufferMemory.cvPixelBuffer, .ioSurface].map {
                ScreenCapture.FormatCaps(format: format, memory: $0, modifiers: [], copies: [])
            }
        }
        return ScreenCapture.BackendCaps(
            backend: backend, sourceKinds: sck ? [.monitor, .application] : [.application], listing: .pickerOnly,
            consent: sck ? .pickerPerSession : .perSessionSystemAlert, video: video, maxFps: nil, minIntervalUs: nil,
            maxSize: nil, scaling: sck ? .native : .unsupported, region: .inMeta,
            composite: ScreenCapture.CompositeCaps(screenshot: .unsupported, streaming: .unsupported),
            cursor: [.hidden], damage: sck ? .rects : .unsupported, orientation: .inMeta,
            colour: ScreenCapture.ColourCaps(colours: [ScreenCapture.Colour(space: .srgb, transfer: .srgb, range: .full)], hdr: false),
            audio: audioCaps(backend), producer: nil, sync: [.ready], persistence: .unsupported, concurrency: 1,
            pool: pool, copies: [.exhaustionReadback], protectedContent: .maskedUndetectable,
            window: ScreenCapture.WindowCaptureCaps(occluded: false, minimised: false, childPopups: false, border: false),
            provenance: ScreenCapture.CapsProvenance(
                sourceKinds: .table, listing: .table, video: sck ? .table : .unknownUntilNegotiated, cursor: .table,
                damage: .table, colour: .table, audio: .table, producer: .unknownUntilNegotiated, sync: .table,
                persistence: .table),
            notes: [])
    }

    static func negotiated(backend: ScreenCapture.BackendId, source: ScreenCapture.Source, size: ScreenCapture.Size,
                           shape: ScreenCaptureShape, fps: ScreenCapture.FrameRate, poolDepth: UInt8,
                           exhaustion: ScreenCapture.Exhaustion,
                           audio: ScreenCapture.AudioNegotiated? = nil) -> ScreenCapture.NegotiatedStream {
        ScreenCapture.NegotiatedStream(
            backend: backend, source: source, size: size,
            contentRect: ScreenCapture.Rect(x: 0, y: 0, width: size.width, height: size.height),
            format: shape.format, memory: shape.memory, modifier: nil, tier: shape.tier, copies: [], fps: fps,
            sourceRateMhz: nil, producer: nil, sync: .ready, poolDepth: poolDepth, exhaustion: exhaustion,
            orientation: .inMeta, crop: .inMeta, damage: backend == .iosSck ? .rects : .unsupported, cursor: .hidden,
            colour: shape.colour, hdr: false, composite: nil,
            consent: backend == .iosSck ? .pickerPerSession : .perSessionSystemAlert,
            indicator: .shown, border: .notApplicable, restoreToken: nil, restored: false, audio: audio)
    }

    static func gravity(_ fit: ScreenCapture.PreviewFit) -> AVLayerVideoGravity {
        switch fit {
        case .fit: return .resizeAspect
        case .fill: return .resizeAspectFill
        case .stretch: return .resize
        }
    }
}

/// What every frame of a session shares.
struct ScreenCaptureShape {
    var format: ScreenCapture.PixelFormat
    var memory: ScreenCapture.BufferMemory
    var colour: ScreenCapture.Colour
    var tier: UInt8
    var sourceId: ScreenCapture.SourceId

    /// `readback`: the frame was copied out (`CopyStage.exhaustionReadback`)
    /// into CPU memory laid out as these planes.
    func meta(sample: CMSampleBuffer, pixels: ScreenCapturePixels, info: ScreenCaptureFrameInfo, seq: UInt64,
              buffer: ScreenCapture.BufferIdentity, skipped: UInt32,
              readback: [ScreenCapture.PlaneLayout]? = nil) -> ScreenCapture.FrameMeta {
        let pts = ScreenCaptureClock.nanoseconds(CMSampleBufferGetPresentationTimeStamp(sample)) ?? ScreenCaptureClock.hostNow()
        return ScreenCapture.FrameMeta(
            seq: seq, ptsNs: pts, ptsClock: .hostTime, wallUs: ScreenCaptureClock.wallUs(forHostNs: pts),
            size: pixels.size, contentRect: info.contentRect(in: pixels.size), format: format,
            memory: readback == nil ? memory : .cpu, planes: readback ?? pixels.planes, modifier: nil,
            buffer: buffer, tier: tier, copied: readback == nil ? nil : .exhaustionReadback, producer: nil,
            colour: colour, hdr: nil, orientation: info.pluginOrientation, damage: info.damage(in: pixels.size),
            cursor: nil, sync: .ready, scalePermille: info.scalePermille, sourceId: sourceId, repeat: false,
            skippedBefore: skipped, protectedContent: false, composite: nil)
    }
}

/// Takes the first complete frame of a capture (screenshots).
final class ScreenCaptureOneShot: ScreenCaptureFeed, @unchecked Sendable {
    typealias Outcome = Result<(CMSampleBuffer, ScreenCaptureFrameInfo), ScreenCapture.CaptureError>

    private let backend: ScreenCapture.BackendId
    private let lock = NSLock()
    private var outcome: Outcome?
    private var waiter: CheckedContinuation<Outcome, Never>?

    init(backend: ScreenCapture.BackendId) { self.backend = backend }

    private func finish(_ result: Outcome) {
        let waiter: CheckedContinuation<Outcome, Never>? = lock.withLock {
            guard outcome == nil else { return nil }
            outcome = result
            defer { self.waiter = nil }
            return self.waiter
        }
        waiter?.resume(returning: result)
    }

    func wait(timeoutNs: UInt64) async -> Outcome {
        let backend = self.backend
        Task { [weak self] in
            try? await Task.sleep(nanoseconds: timeoutNs)
            self?.finish(.failure(ScreenCaptureErrors.detail(backend, "screenshot", "no frame arrived")))
        }
        return await withCheckedContinuation { (continuation: CheckedContinuation<Outcome, Never>) in
            let ready: Outcome? = lock.withLock {
                if let outcome { return outcome }
                waiter = continuation
                return nil
            }
            if let ready { continuation.resume(returning: ready) }
        }
    }

    func video(_ sample: CMSampleBuffer, info: ScreenCaptureFrameInfo) {
        guard info.status == nil || info.status == 0, ScreenCapturePixels(sample) != nil else { return }
        finish(.success((sample, info)))
    }

    func audio(_ sample: CMSampleBuffer) {}
    func paused(_ reason: ScreenCapture.PauseReason) {}
    func resumed() {}
    func stopped(_ error: ScreenCapture.CaptureError) { finish(.failure(error)) }
}

/// One live session: paces and hands frames to the sink (never the
/// producer's last free buffer), feeds the preview layer, delivers audio
/// and fans out events.
final class ScreenCaptureLiveSession: ScreenCaptureFeed, @unchecked Sendable {
    private let backend: ScreenCapture.BackendId
    private let capturer: ScreenCapturer
    private let video: IstmoFrameSender<ScreenCapture.FrameMeta>?
    private let audioOut: ScreenCaptureAudioOut?
    private let layer: AVSampleBufferDisplayLayer?
    private let ledger: ScreenCaptureLedger
    private let exhaustion: ScreenCapture.Exhaustion
    private let events = ScreenCaptureEventHub<ScreenCapture.CaptureEvent>()
    private let lock = NSLock()
    private var request: ScreenCaptureRequest
    private var shape: ScreenCaptureShape
    /// What the person picked; `nil` until the capture runs.
    private var source: ScreenCapture.Source?
    private var audioNegotiated: ScreenCapture.AudioNegotiated?
    private var paused = false
    private var stopped = false
    private var seq: UInt64 = 0
    private var skipped: UInt32 = 0
    private var lastPush: UInt64?
    private var size: ScreenCapture.Size?
    private var slots: [UInt32: UInt32] = [:]
    private var generation: UInt64

    private static let generations = ScreenCaptureGenerations()
    /// Most distinct surfaces one pool generation may show (twice the
    /// deepest queue), as on macOS.
    private static let maxSurfaces = 2 * Int(ScreenCaptureTables.pool.maxDepth)

    init(backend: ScreenCapture.BackendId, capturer: ScreenCapturer, request: ScreenCaptureRequest,
         video: IstmoFrameSender<ScreenCapture.FrameMeta>?, audio: ScreenCaptureAudioOut?,
         layer: AVSampleBufferDisplayLayer?, chosen: ScreenCaptureChoice,
         exhaustion: ScreenCapture.Exhaustion, depth: UInt8) {
        self.backend = backend
        self.capturer = capturer
        self.request = request
        self.video = video
        audioOut = audio
        self.layer = layer
        ledger = ScreenCaptureLedger(depth: Int(depth))
        self.exhaustion = exhaustion
        generation = Self.generations.next()
        shape = ScreenCaptureShape(format: chosen.format, memory: chosen.memory,
                                   colour: ScreenCaptureFourcc.colour(chosen.format, .limited), tier: chosen.tier,
                                   sourceId: ScreenCapture.SourceId(backend: backend, value: "picked"))
    }

    /// The stream as negotiated so far; call with `lock` held.
    private func negotiatedLocked() -> ScreenCapture.NegotiatedStream? {
        guard let source else { return nil }
        let size = self.size ?? source.physicalSize ?? ScreenCapture.Size(width: 0, height: 0)
        return ScreenCaptureTables.negotiated(
            backend: backend, source: source, size: size, shape: shape, fps: request.fps,
            poolDepth: UInt8(clamping: ledger.depth), exhaustion: exhaustion, audio: audioNegotiated)
    }

    /// The stream as negotiated once the person picked; `started` is the
    /// first event every subscriber sees.
    func begin(source: ScreenCapture.Source, audio: ScreenCapture.AudioNegotiated?) -> ScreenCapture.NegotiatedStream {
        lock.withLock {
            shape.sourceId = source.id
            self.source = source
            audioNegotiated = audio
            return negotiatedLocked()!
        }
    }

    func subscribe() -> AsyncThrowingStream<ScreenCapture.CaptureEvent, Error> {
        events.subscribe(first: lock.withLock { negotiatedLocked() }.map { .started($0) })
    }

    private func unmet(_ requirement: ScreenCapture.Requirement) -> ScreenCapture.CaptureError {
        ScreenCaptureBackendImpl.unmet(backend, requirement)
    }

    func update(_ update: ScreenCapture.SessionUpdate) async throws -> ScreenCapture.NegotiatedStream {
        switch update {
        case let .fps(rate):
            var next = lock.withLock { request }
            next.fps = ScreenCaptureChoice.enforced(rate)
            try await capturer.update(next)
            lock.withLock { request = next }
        case .pause:
            lock.withLock { paused = true }
            events.emit(.paused(.requested))
        case .resume:
            lock.withLock { paused = false }
            events.emit(.resumed)
        case let .cursor(cursor):
            if cursor.strength == .require, cursor.mode != .hidden { throw unmet(.cursor) }
        case .region:
            throw unmet(.region)
        case let .formats(formats):
            guard backend == .iosSck else { throw unmet(.operation) }
            guard let choice = ScreenCaptureChoice.negotiate(formats, offers: ScreenCaptureTables.caps(backend).video),
                  let code = ScreenCaptureFourcc.code(for: choice.format) else { throw unmet(.formats) }
            var next = lock.withLock { request }
            next.pixelFormat = code
            try await capturer.update(next)
            lock.withLock {
                request = next
                shape.format = choice.format
                shape.memory = choice.memory
                shape.tier = choice.tier
                shape.colour = ScreenCaptureFourcc.colour(choice.format, .limited)
                generation = Self.generations.next()
                slots = [:]
            }
        }
        guard let stream = lock.withLock({ negotiatedLocked() }) else { throw ScreenCapture.CaptureError.sessionNotFound }
        if case .formats = update { events.emit(.renegotiated(stream)) }
        return stream
    }

    // MARK: Feed

    func video(_ sample: CMSampleBuffer, info: ScreenCaptureFrameInfo) {
        switch info.status {
        case nil, 0?:
            resumed()
        case 3?:
            paused(.systemPaused)
            return
        default:
            // Idle, blank, started, stopped: no new content.
            return
        }
        guard let pixels = ScreenCapturePixels(sample) else { return }
        let current: (shape: ScreenCaptureShape, resized: Bool)? = lock.withLock {
            guard !stopped, !paused else { return nil }
            defer { size = pixels.size }
            let resized = size.map { $0.width != pixels.size.width || $0.height != pixels.size.height } ?? false
            return (shape, resized)
        }
        guard let current else { return }
        if current.resized { events.emit(.sourceResized(pixels.size)) }
        if let layer { Self.enqueue(sample, into: layer) }
        guard let video else { return }
        let now = ScreenCaptureClock.hostNow()
        guard due(now: now, damage: info.damage(in: pixels.size)) else {
            lock.withLock { skipped += 1 }
            return
        }
        var shape = current.shape
        if let (format, range) = ScreenCaptureFourcc.format(pixels.code), format != shape.format {
            // ReplayKit delivers the system's own format.
            shape.format = format
            shape.colour = ScreenCaptureFourcc.colour(format, range)
        }
        let surfaceId = pixels.surface.map { IOSurfaceGetID($0) } ?? 0
        let identity: (seq: UInt64, buffer: ScreenCapture.BufferIdentity, skipped: UInt32) = lock.withLock {
            if current.resized {
                // Buffers of another size: the producer made a new pool.
                slots = [:]
                generation = Self.generations.next()
            }
            if slots[surfaceId] == nil {
                // The pool may be deeper than `queueDepth`: slots grow with
                // the surfaces seen; only past what one pool holds is it a
                // new pool.
                if slots.count >= Self.maxSurfaces {
                    slots = [:]
                    generation = Self.generations.next()
                }
                slots[surfaceId] = UInt32(slots.count)
                ledger.grow(to: slots.count)
            }
            defer {
                seq += 1
                skipped = 0
            }
            return (seq, ScreenCapture.BufferIdentity(poolGeneration: generation, slot: slots[surfaceId] ?? 0), skipped)
        }
        let meta = shape.meta(sample: sample, pixels: pixels, info: info, seq: identity.seq,
                              buffer: identity.buffer, skipped: identity.skipped)
        let status: IstmoSubmitStatus
        if let exhausted = ledger.hold() {
            if exhausted.isNew {
                events.emit(.exhausted(ScreenCapture.ExhaustionReport(
                    episode: exhausted.episode, poolDepth: UInt8(clamping: ledger.depth), policy: exhaustion)))
            }
            // The buffer goes back to the producer at once unless it is
            // read back.
            guard exhaustion == .copyToCpu, let (bytes, planes) = pixels.copyOut() else {
                lock.withLock { skipped += 1 }
                return
            }
            let copied = shape.meta(sample: sample, pixels: pixels, info: info, seq: identity.seq,
                                    buffer: identity.buffer, skipped: identity.skipped, readback: planes)
            status = video.submitBytes(bytes, meta: copied)
        } else if shape.memory == .ioSurface, let surface = pixels.surface {
            status = ScreenCaptureLedgers.submit(surface, kind: .ioSurface, meta: meta, ledger: ledger, to: video)
        } else {
            status = ScreenCaptureLedgers.submit(pixels.buffer, kind: .cvPixelBuffer, meta: meta, ledger: ledger, to: video)
        }
        lock.withLock { lastPush = now }
        if status.shouldStop {
            Task { await self.shutdown(.closed) }
        }
    }

    /// Whether a frame at `now` is due under the session's rate (frames
    /// inside a period are dropped and counted in `skippedBefore`).
    private func due(now: UInt64, damage: ScreenCapture.Damage) -> Bool {
        lock.withLock {
            let rate: UInt32
            switch request.fps {
            case .native:
                return true
            case let .onDamage(n):
                if case .unchanged = damage { return false }
                rate = n
            case let .max(n), let .exact(n):
                rate = n
            }
            let period = 1_000_000_000 / UInt64(max(1, rate))
            guard let last = lastPush, now > last else { return true }
            // A frame within 1/20 of a period of its due time goes out.
            return now - last + period / 20 >= period
        }
    }

    private static func enqueue(_ sample: CMSampleBuffer, into layer: AVSampleBufferDisplayLayer) {
        if #available(iOS 17.0, *) {
            layer.sampleBufferRenderer.enqueue(sample)
        } else {
            layer.enqueue(sample)
        }
    }

    func audio(_ sample: CMSampleBuffer) {
        guard let audioOut, !lock.withLock({ stopped }) else { return }
        if !audioOut.deliver(sample) {
            Task { await self.shutdown(.closed) }
        }
    }

    func paused(_ reason: ScreenCapture.PauseReason) {
        let first: Bool = lock.withLock {
            defer { paused = true }
            return !paused
        }
        if first { events.emit(.paused(reason)) }
    }

    func resumed() {
        let was: Bool = lock.withLock {
            defer { paused = false }
            return paused
        }
        if was { events.emit(.resumed) }
    }

    func stopped(_ error: ScreenCapture.CaptureError) {
        Task { await self.shutdown(.failed, error: error) }
    }

    /// End the session: the capture stops, the sinks end (with the error
    /// when there is one), `stopped` is the last event.
    func shutdown(_ reason: ScreenCapture.StopReason, error: ScreenCapture.CaptureError? = nil) async {
        let first: Bool = lock.withLock {
            defer { stopped = true }
            return !stopped
        }
        guard first else { return }
        await capturer.stop()
        var reason = reason
        if let error {
            switch error {
            case .consentRevoked:
                reason = .consentRevoked
                events.emit(.consentRevoked)
            case .sourceLost:
                reason = .sourceLost
                events.emit(.sourceLost)
            default:
                break
            }
            events.emit(.failed(error))
            video?.close(error: String(describing: error))
        } else {
            video?.close()
        }
        audioOut?.finish(reason, error: error)
        if let layer {
            DispatchQueue.main.async {
                if #available(iOS 17.0, *) {
                    layer.sampleBufferRenderer.flush()
                } else {
                    layer.flush()
                }
            }
        }
        events.emit(.stopped(reason))
        events.end()
    }
}

/// Process-unique pool generations, so buffer identities never repeat.
final class ScreenCaptureGenerations: @unchecked Sendable {
    private let lock = NSLock()
    private var last: UInt64 = 0

    func next() -> UInt64 {
        lock.withLock {
            last += 1
            return last
        }
    }
}

/// An audio sink: interleaved PCM in the asked format, paused (dropped,
/// then a discontinuity) or muted (zeroed, on time), with its events.
final class ScreenCaptureAudioOut: @unchecked Sendable {
    let negotiated: ScreenCapture.AudioNegotiated
    let events = ScreenCaptureEventHub<ScreenCapture.AudioEvent>()
    private let sender: IstmoFrameSender<ScreenCapture.AudioMeta>
    private let lock = NSLock()
    private var seq: UInt64 = 0
    private var paused = false
    private var muted = false
    private var gap = false
    private var nextPts: UInt64?
    private var ended = false

    init(sender: IstmoFrameSender<ScreenCapture.AudioMeta>, negotiated: ScreenCapture.AudioNegotiated) {
        self.sender = sender
        self.negotiated = negotiated
    }

    /// `false` once the sink is gone.
    func deliver(_ sample: CMSampleBuffer) -> Bool {
        guard let chunk = ScreenCapturePcm.chunk(sample, format: negotiated.format) else { return true }
        let pts = ScreenCaptureClock.nanoseconds(CMSampleBufferGetPresentationTimeStamp(sample)) ?? ScreenCaptureClock.hostNow()
        let duration = UInt64(chunk.frames) * 1_000_000_000 / UInt64(max(1, chunk.rate))
        let state: (seq: UInt64, discontinuity: Bool, muted: Bool)? = lock.withLock {
            guard !ended else { return nil }
            // A chunk that does not start where the previous one ended
            // (more than 100 ms off) breaks the timeline.
            let broken = nextPts.map { $0 > pts ? $0 - pts > 100_000_000 : pts - $0 > 100_000_000 } ?? false
            nextPts = pts + duration
            if paused {
                gap = true
                return nil
            }
            defer {
                seq += 1
                gap = false
            }
            return (seq, broken || gap, muted)
        }
        guard let state else { return !sender.isClosed }
        let bytes = state.muted ? Data([UInt8](repeating: 0, count: chunk.bytes.count)) : chunk.bytes
        let meta = ScreenCapture.AudioMeta(
            seq: state.seq, ptsNs: pts, wallUs: ScreenCaptureClock.wallUs(forHostNs: pts), rate: chunk.rate,
            channels: chunk.channels, format: negotiated.format, frames: chunk.frames,
            discontinuity: state.discontinuity)
        return !sender.submitBytes(bytes, meta: meta).shouldStop
    }

    func apply(_ update: ScreenCapture.AudioUpdate) -> ScreenCapture.AudioNegotiated {
        let event: ScreenCapture.AudioEvent
        switch update {
        case .pause:
            lock.withLock { paused = true }
            event = .paused(.requested)
        case .resume:
            lock.withLock { paused = false }
            event = .resumed
        case .mute:
            lock.withLock { muted = true }
            event = .muted
        case .unmute:
            lock.withLock { muted = false }
            event = .unmuted
        }
        events.emit(event)
        return negotiated
    }

    func finish(_ reason: ScreenCapture.StopReason, error: ScreenCapture.CaptureError?) {
        let first: Bool = lock.withLock {
            defer { ended = true }
            return !ended
        }
        guard first else { return }
        if let error {
            events.emit(.failed(error))
            sender.close(error: String(describing: error))
        } else {
            sender.close()
        }
        events.emit(.stopped(reason))
        events.end()
    }
}

/// An audio-only session.
final class ScreenCaptureAudioSession: ScreenCaptureFeed, @unchecked Sendable {
    let capturer: ScreenCapturer
    let out: ScreenCaptureAudioOut

    init(capturer: ScreenCapturer, out: ScreenCaptureAudioOut) {
        self.capturer = capturer
        self.out = out
    }

    func video(_ sample: CMSampleBuffer, info: ScreenCaptureFrameInfo) {}

    func audio(_ sample: CMSampleBuffer) {
        if !out.deliver(sample) {
            Task { await self.capturer.stop() }
        }
    }

    func paused(_ reason: ScreenCapture.PauseReason) { out.events.emit(.paused(reason)) }
    func resumed() { out.events.emit(.resumed) }

    func stopped(_ error: ScreenCapture.CaptureError) {
        Task {
            await self.capturer.stop()
            self.out.finish(.failed, error: error)
        }
    }
}
