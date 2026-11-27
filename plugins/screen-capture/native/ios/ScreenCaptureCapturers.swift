import CoreGraphics
import CoreMedia
import Foundation
import IstmoRuntime
import ReplayKit
import UIKit
#if canImport(ScreenCaptureKit)
import ScreenCaptureKit
#endif

// The two iOS capture paths behind one interface: ScreenCaptureKit
// in-process (iOS / iPadOS 27, the system picker) and ReplayKit in-app
// capture (before 27, a system alert per capture, foreground only).

/// Where a running capture reports. Called on the capturer's queue.
protocol ScreenCaptureFeed: AnyObject {
    func video(_ sample: CMSampleBuffer, info: ScreenCaptureFrameInfo)
    func audio(_ sample: CMSampleBuffer)
    func paused(_ reason: ScreenCapture.PauseReason)
    func resumed()
    func stopped(_ error: ScreenCapture.CaptureError)
}

/// What to capture, as a capturer needs it.
struct ScreenCaptureRequest {
    /// The Core Video format to ask for (`nil`: the system's own).
    var pixelFormat: OSType?
    var fps: ScreenCapture.FrameRate
    var size: ScreenCapture.OutputSize
    var queueDepth: Int
    var video: Bool
    var audio: ScreenCaptureAudioRequest?
    /// Only this app's own content (a picker asked for applications
    /// only): ScreenCaptureKit presents its picker for the current app.
    var appOnly = false
}

struct ScreenCaptureAudioRequest {
    var rate: UInt32
    var channels: UInt8
    var excludeSelf: Bool
}

protocol ScreenCapturer: AnyObject {
    var backend: ScreenCapture.BackendId { get }
    /// Show the system UI (picker or alert), then capture into `feed`.
    /// Returns what the person picked.
    func start(_ request: ScreenCaptureRequest, feed: ScreenCaptureFeed) async throws -> ScreenCapture.Source
    /// Apply a new frame rate / format to the running capture.
    func update(_ request: ScreenCaptureRequest) async throws
    func stop() async
}

enum ScreenCaptureErrors {
    static func detail(_ backend: ScreenCapture.BackendId, _ step: String, _ message: String) -> ScreenCapture.CaptureError {
        .protocol(ScreenCapture.BackendDetail(backend: backend, step: step, message: message))
    }

    /// The plugin error a ReplayKit / ScreenCaptureKit error stands for.
    static func of(_ error: Error, backend: ScreenCapture.BackendId, step: String) -> ScreenCapture.CaptureError {
        if let error = error as? ScreenCapture.CaptureError { return error }
        if let error = error as? RPRecordingError {
            switch error.code {
            case .userDeclined: return .consentDenied
            case .interrupted, .systemDormancy, .activePhoneCall: return .sourceLost
            default:
                return .os(ScreenCapture.OsFailure(backend: backend, operation: step, code: Int64(error.code.rawValue)))
            }
        }
        #if canImport(ScreenCaptureKit)
        if #available(iOS 27.0, *), let error = error as? SCStreamError {
            switch error.code {
            case .userDeclined: return .consentDenied
            case .userStopped: return .consentRevoked
            case .noCaptureSource, .noDisplayList, .noWindowList, .removingStream, .systemStoppedStream:
                return .sourceLost
            default:
                return .os(ScreenCapture.OsFailure(backend: backend, operation: step, code: Int64(error.code.rawValue)))
            }
        }
        #endif
        return detail(backend, step, String(describing: error))
    }
}

// MARK: ReplayKit (iOS < 27, legacy)

/// `RPScreenRecorder` in-app capture: the app's own screen, after the
/// system alert; capture stops while the app is in the background.
/// Buffers come from a system pool (holding them stalls capture);
/// timestamps are host time.
final class ReplayKitCapturer: ScreenCapturer, @unchecked Sendable {
    let backend: ScreenCapture.BackendId = .iosReplayKit
    private let lock = NSLock()
    private var observers: [NSObjectProtocol] = []

    func start(_ request: ScreenCaptureRequest, feed: ScreenCaptureFeed) async throws -> ScreenCapture.Source {
        let recorder = RPScreenRecorder.shared()
        guard recorder.isAvailable else {
            throw ScreenCapture.CaptureError.backendUnavailable(ScreenCapture.BackendReason(
                backend: backend, availability: .notOnThisSession("ReplayKit screen recording is unavailable")))
        }
        guard !recorder.isRecording else {
            throw ScreenCaptureErrors.detail(backend, "start", "another ReplayKit capture is running")
        }
        recorder.isMicrophoneEnabled = false
        let backend = self.backend
        let wantsAudio = request.audio != nil
        let wantsVideo = request.video
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            recorder.startCapture(handler: { [weak feed] sample, type, error in
                guard let feed else { return }
                if let error {
                    feed.stopped(ScreenCaptureErrors.of(error, backend: backend, step: "capture"))
                    return
                }
                switch type {
                case .video where wantsVideo:
                    var info = ScreenCaptureFrameInfo()
                    let key = RPVideoSampleOrientationKey as CFString
                    if let value = CMGetAttachment(sample, key: key, attachmentModeOut: nil) as? NSNumber {
                        info.orientation = value.uint32Value
                    }
                    feed.video(sample, info: info)
                case .audioApp where wantsAudio:
                    feed.audio(sample)
                default:
                    break
                }
            }, completionHandler: { error in
                if let error {
                    continuation.resume(throwing: ScreenCaptureErrors.of(error, backend: backend, step: "start"))
                } else {
                    continuation.resume()
                }
            })
        }
        let center = NotificationCenter.default
        let background = center.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: nil
        ) { [weak feed] _ in feed?.paused(.backgrounded) }
        let foreground = center.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: nil
        ) { [weak feed] _ in feed?.resumed() }
        lock.withLock { observers = [background, foreground] }
        return ScreenCapture.Source(
            id: ScreenCapture.SourceId(backend: backend, value: "app"), kind: .application, name: "this app",
            appId: nil, pid: nil, geometry: nil, physicalSize: nil, scalePermille: nil, refreshMhz: nil,
            orientation: ScreenCapture.Orientation(mirrored: false, rotation: .r0),
            flags: ScreenCapture.SourceFlags(primary: true, minimised: false, occluded: false, protected: false, ownProcess: true),
            make: nil, model: nil, connector: nil, description: nil, capture: .pickerOnly([backend]))
    }

    func update(_ request: ScreenCaptureRequest) async throws {
        // Rate and format are the system's; the session paces frames itself.
    }

    func stop() async {
        let center = NotificationCenter.default
        for observer in lock.withLock({ () -> [NSObjectProtocol] in
            defer { observers = [] }
            return observers
        }) {
            center.removeObserver(observer)
        }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            RPScreenRecorder.shared().stopCapture { _ in continuation.resume() }
        }
    }
}

// MARK: ScreenCaptureKit (iOS / iPadOS 27)

#if canImport(ScreenCaptureKit)
/// ScreenCaptureKit in-process: the shared content-sharing picker, then an
/// `SCStream` on the chosen filter (the full display, kept running in the
/// background with the `screen-capture` background mode, or the app's
/// own content). Sample buffers are `IOSurface`-backed, on the host clock.
@available(iOS 27.0, *)
final class SckCapturer: NSObject, ScreenCapturer, SCStreamOutput, SCStreamDelegate,
    SCContentSharingPickerObserver, @unchecked Sendable {
    let backend: ScreenCapture.BackendId = .iosSck
    private let lock = NSLock()
    private let queue = DispatchQueue(label: "dev.istmo.screen-capture.sck", qos: .userInitiated)
    private var stream: SCStream?
    private var filter: SCContentFilter?
    private weak var feed: ScreenCaptureFeed?
    /// The pick waiting for the person, and its number (a timeout or a
    /// cancellation only ends the pick it was armed for).
    private var pick: (id: UInt64, continuation: CheckedContinuation<SCContentFilter, Error>)?
    private var picks: UInt64 = 0
    /// Longest a person may take to pick, as on macOS.
    private static let pickTimeoutNs: UInt64 = 120_000_000_000
    /// App life-cycle observers, removed on stop.
    private var observers: [NSObjectProtocol] = []

    override init() {
        super.init()
    }

    func start(_ request: ScreenCaptureRequest, feed: ScreenCaptureFeed) async throws -> ScreenCapture.Source {
        let filter = try await choose(appOnly: request.appOnly)
        let stream = SCStream(filter: filter, configuration: configuration(request, filter: filter), delegate: self)
        if request.video {
            try stream.addStreamOutput(self, type: .screen, sampleHandlerQueue: queue)
        }
        if request.audio != nil {
            try stream.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        }
        lock.withLock {
            self.stream = stream
            self.filter = filter
            self.feed = feed
        }
        let backend = self.backend
        do {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
                stream.startCapture { error in
                    if let error {
                        continuation.resume(throwing: ScreenCaptureErrors.of(error, backend: backend, step: "start"))
                    } else {
                        continuation.resume()
                    }
                }
            }
        } catch {
            // Let go of the stream (no capturer ↔ stream cycle) and stop
            // whatever did start.
            await stop()
            lock.withLock { self.filter = nil }
            throw error
        }
        observeBackground(feed: feed, style: filter.style)
        let scale = Double(filter.pointPixelScale)
        let rect = filter.contentRect
        return ScreenCapture.Source(
            id: ScreenCapture.SourceId(backend: backend, value: "picked"), kind: Self.kind(of: filter), name: "picked content",
            appId: nil, pid: nil,
            geometry: ScreenCaptureFrameInfo.covering(rect, scale: 1),
            physicalSize: ScreenCaptureFrameInfo.covering(rect, scale: scale).map {
                ScreenCapture.Size(width: $0.width, height: $0.height)
            },
            scalePermille: UInt32(max(1, (scale * 1000).rounded())), refreshMhz: nil,
            orientation: ScreenCapture.Orientation(mirrored: false, rotation: .r0),
            flags: ScreenCapture.SourceFlags(primary: true, minimised: false, occluded: false, protected: false, ownProcess: false),
            make: nil, model: nil, connector: nil, description: nil, capture: .pickerOnly([backend]))
    }

    /// What the person shared, by the filter's style: the display, or an
    /// app's content (iOS has no windows to pick).
    private static func kind(of filter: SCContentFilter) -> ScreenCapture.SourceKind {
        switch filter.style {
        case .application, .window: return .application
        case .display, .none: return .monitor
        @unknown default: return .monitor
        }
    }

    /// Whether the app declares the `screen-capture` background mode
    /// (`UIBackgroundModes`), which keeps a full-display capture running
    /// in the background.
    private static var capturesInBackground: Bool {
        let modes = Bundle.main.object(forInfoDictionaryKey: "UIBackgroundModes") as? [String]
        return modes?.contains("screen-capture") ?? false
    }

    /// Report `paused(.backgrounded)` while the app is in the background,
    /// unless the capture keeps running there (the display, with the
    /// `screen-capture` background mode); `resumed` back in the foreground.
    private func observeBackground(feed: ScreenCaptureFeed, style: SCShareableContentStyle) {
        if style == .display, Self.capturesInBackground { return }
        let center = NotificationCenter.default
        let background = center.addObserver(
            forName: UIApplication.didEnterBackgroundNotification, object: nil, queue: nil
        ) { [weak feed] _ in feed?.paused(.backgrounded) }
        let foreground = center.addObserver(
            forName: UIApplication.willEnterForegroundNotification, object: nil, queue: nil
        ) { [weak feed] _ in feed?.resumed() }
        lock.withLock { observers = [background, foreground] }
    }

    /// Show the shared picker (for this app's content only with `appOnly`);
    /// the person's choice comes back through the observer methods below.
    /// The pick ends with the person's choice, after `pickTimeoutNs`, or
    /// when the calling task is cancelled (all `consentRequired`); a new
    /// pick on this capturer ends the one before it.
    private func choose(appOnly: Bool) async throws -> SCContentFilter {
        let id: UInt64 = lock.withLock {
            picks += 1
            return picks
        }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<SCContentFilter, Error>) in
                let previous = lock.withLock { () -> CheckedContinuation<SCContentFilter, Error>? in
                    defer { pick = (id, continuation) }
                    return pick?.continuation
                }
                previous?.resume(throwing: ScreenCapture.CaptureError.consentRequired)
                if Task.isCancelled {
                    finishPick(.failure(ScreenCapture.CaptureError.consentRequired), id: id)
                    return
                }
                DispatchQueue.main.async {
                    let picker = SCContentSharingPicker.shared
                    picker.add(self)
                    picker.isActive = true
                    if appOnly {
                        picker.presentForCurrentApplication()
                    } else {
                        picker.present()
                    }
                }
                Task { [weak self] in
                    try? await Task.sleep(nanoseconds: Self.pickTimeoutNs)
                    self?.finishPick(.failure(ScreenCapture.CaptureError.consentRequired), id: id)
                }
            }
        } onCancel: {
            finishPick(.failure(ScreenCapture.CaptureError.consentRequired), id: id)
        }
    }

    /// End the waiting pick (only pick `id`, when given).
    private func finishPick(_ outcome: Result<SCContentFilter, Error>, id: UInt64? = nil) {
        guard let continuation = lock.withLock({ () -> CheckedContinuation<SCContentFilter, Error>? in
            guard let pending = pick, id == nil || pending.id == id else { return nil }
            pick = nil
            return pending.continuation
        }) else { return }
        DispatchQueue.main.async {
            let picker = SCContentSharingPicker.shared
            picker.remove(self)
            picker.isActive = false
        }
        continuation.resume(with: outcome)
    }

    func contentSharingPicker(_ picker: SCContentSharingPicker, didCancelFor stream: SCStream?) {
        finishPick(.failure(ScreenCapture.CaptureError.consentDenied))
    }

    func contentSharingPicker(_ picker: SCContentSharingPicker, didUpdateWith filter: SCContentFilter, for stream: SCStream?) {
        finishPick(.success(filter))
    }

    func contentSharingPickerStartDidFailWithError(_ error: Error) {
        finishPick(.failure(ScreenCaptureErrors.of(error, backend: backend, step: "picker")))
    }

    private func configuration(_ request: ScreenCaptureRequest, filter: SCContentFilter) -> SCStreamConfiguration {
        let config = SCStreamConfiguration()
        let scale = Double(filter.pointPixelScale)
        if let native = ScreenCaptureFrameInfo.covering(filter.contentRect, scale: scale) {
            let size = ScreenCaptureSizes.output(request.size,
                                                 native: ScreenCapture.Size(width: native.width, height: native.height))
            config.width = Int(size.width)
            config.height = Int(size.height)
        }
        if case .native = request.size {} else { config.scalesToFit = true }
        if let format = request.pixelFormat { config.pixelFormat = format }
        config.minimumFrameInterval = ScreenCaptureSizes.interval(request.fps)
        config.queueDepth = request.queueDepth
        config.showsCursor = false
        if let audio = request.audio {
            config.capturesAudio = true
            config.sampleRate = Int(audio.rate)
            config.channelCount = Int(audio.channels)
            config.excludesCurrentProcessAudio = audio.excludeSelf
        }
        return config
    }

    func update(_ request: ScreenCaptureRequest) async throws {
        guard let (stream, filter) = lock.withLock({ () -> (SCStream, SCContentFilter)? in
            guard let stream = self.stream, let filter = self.filter else { return nil }
            return (stream, filter)
        }) else { throw ScreenCapture.CaptureError.sessionNotFound }
        let config = configuration(request, filter: filter)
        let backend = self.backend
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, Error>) in
            stream.updateConfiguration(config) { error in
                if let error {
                    continuation.resume(throwing: ScreenCaptureErrors.of(error, backend: backend, step: "update"))
                } else {
                    continuation.resume()
                }
            }
        }
    }

    func stop() async {
        let center = NotificationCenter.default
        for observer in lock.withLock({ () -> [NSObjectProtocol] in
            defer { observers = [] }
            return observers
        }) {
            center.removeObserver(observer)
        }
        guard let stream = lock.withLock({ () -> SCStream? in
            defer { self.stream = nil; self.feed = nil }
            return self.stream
        }) else { return }
        await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
            stream.stopCapture { _ in continuation.resume() }
        }
        // Let a sample handler still running on the queue finish first.
        queue.sync {}
    }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard let feed = lock.withLock({ self.feed }) else { return }
        switch type {
        case .screen:
            feed.video(sampleBuffer, info: Self.info(sampleBuffer))
        case .audio:
            feed.audio(sampleBuffer)
        default:
            break
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        lock.withLock { self.feed }?.stopped(ScreenCaptureErrors.of(error, backend: backend, step: "stream"))
    }

    /// The frame info SCK attaches to a screen sample.
    private static func info(_ sample: CMSampleBuffer) -> ScreenCaptureFrameInfo {
        var info = ScreenCaptureFrameInfo()
        guard let all = CMSampleBufferGetSampleAttachmentsArray(sample, createIfNecessary: false) as? [[SCStreamFrameInfo: Any]],
              let first = all.first else { return info }
        info.status = first[.status] as? Int
        if let dictionary = first[.contentRect] as? NSDictionary {
            info.contentRect = CGRect(dictionaryRepresentation: dictionary as CFDictionary)
        }
        info.scaleFactor = (first[.scaleFactor] as? NSNumber)?.doubleValue
        info.contentScale = (first[.contentScale] as? NSNumber)?.doubleValue
        if let rects = first[.dirtyRects] as? [Any] {
            info.dirtyRects = rects.compactMap { value -> CGRect? in
                if let value = value as? NSDictionary { return CGRect(dictionaryRepresentation: value as CFDictionary) }
                return (value as? NSValue)?.cgRectValue
            }
        }
        info.orientation = (first[.videoOrientation] as? NSNumber)?.uint32Value
        return info
    }
}
#endif

/// Sizes and intervals shared by both capturers.
enum ScreenCaptureSizes {
    /// The size to ask for: native, as asked, or fitted keeping the aspect.
    static func output(_ requested: ScreenCapture.OutputSize, native: ScreenCapture.Size) -> ScreenCapture.Size {
        switch requested {
        case .native:
            return native
        case let .scaled(size):
            return size
        case let .fit(bound):
            guard native.width > 0, native.height > 0, bound.width > 0, bound.height > 0 else { return bound }
            let (nw, nh) = (UInt64(native.width), UInt64(native.height))
            let (bw, bh) = (UInt64(bound.width), UInt64(bound.height))
            if nw * bh >= nh * bw {
                return ScreenCapture.Size(width: bound.width, height: UInt32(max(1, (nh * bw + nw - 1) / nw)))
            }
            return ScreenCapture.Size(width: UInt32(max(1, (nw * bh + nh - 1) / nh)), height: bound.height)
        }
    }

    /// `minimumFrameInterval` for a rate; zero is the display's own.
    static func interval(_ fps: ScreenCapture.FrameRate) -> CMTime {
        switch fps {
        case .native: return .zero
        case let .max(n), let .exact(n), let .onDamage(n): return CMTime(value: 1, timescale: Int32(clamping: max(1, n)))
        }
    }
}
