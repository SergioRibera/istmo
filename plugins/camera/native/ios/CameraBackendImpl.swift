import AVFoundation
import CoreMedia
import CoreVideo
import Foundation
import IstmoRuntime

/// AVFoundation-backed implementation of `CameraBackend`.
///
/// One `CameraBackendImpl` instance may own several sessions
/// concurrently — iOS A12+ exposes `AVCaptureMultiCamSession`
/// (`isMultiCamSupported` is checked in `concurrent_support`).
///
/// Frames are produced on `AVCaptureVideoDataOutput`'s sample buffer
/// delegate queue and fanned out to every subscriber of
/// `frames(session:)`; each subscriber owns its own bounded buffer.
final class CameraBackendImpl: NSObject, CameraBackend, HandleReleaser {

    private let config: CameraInitConfig
    /// Serialises every access to `sessions`. The async protocol methods
    /// run on arbitrary executors and `releaseNativeHandle` arrives on the
    /// runtime's pump thread.
    private let queue = DispatchQueue(label: "istmo.camera.backend", qos: .userInitiated)
    private var sessions: [NativeHandleId: SessionState] = [:]

    init(config: CameraInitConfig) {
        self.config = config
    }

    // MARK: Discovery

    func devices() async throws -> [CameraDevice] {
        let types: [AVCaptureDevice.DeviceType] = [
            .builtInWideAngleCamera,
            .builtInUltraWideCamera,
            .builtInTelephotoCamera,
            .builtInDualCamera,
            .builtInDualWideCamera,
            .builtInTripleCamera,
            .builtInTrueDepthCamera,
            .builtInLiDARDepthCamera,
            .external,
        ]
        let session = AVCaptureDevice.DiscoverySession(
            deviceTypes: types,
            mediaType: .video,
            position: .unspecified
        )
        return session.devices.map { describe(device: $0) }
    }

    func concurrent_support() async throws -> ConcurrentSupport {
        let supported = AVCaptureMultiCamSession.isMultiCamSupported
        return ConcurrentSupport(
            maxConcurrent: supported ? UInt32(4) : UInt32(1),
            approvedPairs: [],
            anyCombination: supported
        )
    }

    // MARK: Permissions

    func access_status() async throws -> AccessStatus {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .authorized: return .granted
        case .denied: return .denied
        case .notDetermined: return .notDetermined
        case .restricted: return .restricted
        @unknown default: return .notDetermined
        }
    }

    func request_access() async throws -> AccessStatus {
        let granted: Bool = await withCheckedContinuation { cont in
            AVCaptureDevice.requestAccess(for: .video) { cont.resume(returning: $0) }
        }
        return granted ? .granted : .denied
    }

    // MARK: Session lifecycle

    func open(config: SessionConfig) async throws -> CameraSession {
        switch AVCaptureDevice.authorizationStatus(for: .video) {
        case .denied, .restricted: throw CameraError.permissionDenied
        default: break
        }
        guard let device = AVCaptureDevice(uniqueID: config.deviceId) else {
            throw CameraError.deviceNotFound(config.deviceId)
        }
        guard let captureType = pixelFormatType(for: config.pixelFormat) else {
            throw CameraError.formatUnsupported
        }

        let captureSession: AVCaptureSession = AVCaptureMultiCamSession.isMultiCamSupported
            ? AVCaptureMultiCamSession()
            : AVCaptureSession()
        captureSession.beginConfiguration()

        let input: AVCaptureDeviceInput
        do {
            input = try AVCaptureDeviceInput(device: device)
        } catch {
            captureSession.commitConfiguration()
            throw CameraError.backend("cannot open camera input: \(error.localizedDescription)")
        }
        guard captureSession.canAddInput(input) else {
            captureSession.commitConfiguration()
            throw CameraError.backend("cannot add camera input")
        }
        captureSession.addInput(input)

        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        guard output.availableVideoPixelFormatTypes.contains(captureType) else {
            captureSession.commitConfiguration()
            throw CameraError.formatUnsupported
        }
        output.videoSettings = [kCVPixelBufferPixelFormatTypeKey as String: captureType]
        guard captureSession.canAddOutput(output) else {
            captureSession.commitConfiguration()
            throw CameraError.backend("cannot add video output")
        }
        captureSession.addOutput(output)

        // Optional: a session without a photo output still streams, and
        // `capture_still` then reports `formatUnsupported`.
        let photoOutput = AVCapturePhotoOutput()
        let hasPhotoOutput = captureSession.canAddOutput(photoOutput)
        if hasPhotoOutput {
            captureSession.addOutput(photoOutput)
        }

        if let res = config.resolution {
            let preset = closestPreset(width: res.width, height: res.height)
            if captureSession.canSetSessionPreset(preset) {
                captureSession.sessionPreset = preset
            }
        }

        let fanout = FrameFanout(depth: Int(max(self.config.framePoolDepth, 1)))
        let sampleQueue = DispatchQueue(label: "istmo.camera.samples")
        var delegate: SampleDelegate?
        if config.emitFrames {
            let sampleDelegate = SampleDelegate(pixelFormat: config.pixelFormat, fanout: fanout)
            output.setSampleBufferDelegate(sampleDelegate, queue: sampleQueue)
            delegate = sampleDelegate
        }
        captureSession.commitConfiguration()

        // The preset picks the active format, so the frame rate is set
        // once configuration is committed.
        if let fps = config.targetFps {
            setFrameRate(fps, on: device)
        }
        do {
            try apply(initial: config.initialControls, to: device)
        } catch {
            throw CameraError.backend("cannot apply initial controls: \(error.localizedDescription)")
        }

        captureSession.startRunning()

        let handleId = IstmoRuntime.shared.allocHandleId(pluginId: "istmo.camera")
        let state = SessionState(
            captureSession: captureSession,
            device: device,
            output: output,
            photoOutput: hasPhotoOutput ? photoOutput : nil,
            delegate: delegate,
            fanout: fanout
        )
        queue.sync { sessions[handleId] = state }

        let dims = CMVideoFormatDescriptionGetDimensions(device.activeFormat.formatDescription)
        return CameraSession(
            session: handleId,
            negotiated: NegotiatedFormat(
                resolution: Resolution(width: UInt32(dims.width), height: UInt32(dims.height)),
                pixelFormat: config.pixelFormat,
                fps: currentFrameRate(of: device),
                rotationDegrees: 0
            )
        )
    }

    func close(session: NativeHandleId) async throws {
        closeSession(session)
    }

    func frames(session: NativeHandleId) -> AsyncThrowingStream<CameraFrame, Error> {
        guard let fanout = queue.sync(execute: { sessions[session]?.fanout }) else {
            return AsyncThrowingStream { $0.finish(throwing: CameraError.sessionClosed) }
        }
        return fanout.subscribe()
    }

    func releaseNativeHandle(_ handleId: UInt64) {
        // Every release in the runtime reaches every releaser; ids this
        // backend never handed out are not in `sessions` and no-op.
        closeSession(handleId)
    }

    private func closeSession(_ id: NativeHandleId) {
        guard let state = queue.sync(execute: { sessions.removeValue(forKey: id) }) else { return }
        state.output.setSampleBufferDelegate(nil, queue: nil)
        state.captureSession.stopRunning()
        state.fanout.finish()
        IstmoRuntime.shared.forgetHandle(id)
    }

    private func lookup(_ id: NativeHandleId) throws -> SessionState {
        guard let state = queue.sync(execute: { sessions[id] }) else {
            throw CameraError.sessionClosed
        }
        return state
    }

    // MARK: Controls

    func apply_controls(session: NativeHandleId, controls: CameraControls) async throws -> AppliedControls {
        let device = try lookup(session).device
        do {
            try device.lockForConfiguration()
        } catch {
            throw CameraError.backend("cannot lock camera configuration: \(error.localizedDescription)")
        }
        defer { device.unlockForConfiguration() }

        if let zoom = controls.zoom {
            let clamped = min(
                max(CGFloat(zoom), device.minAvailableVideoZoomFactor),
                device.maxAvailableVideoZoomFactor
            )
            device.videoZoomFactor = clamped
        }
        if let torch = controls.torch, device.hasTorch {
            device.torchMode = torch ? .on : .off
        }
        if let ev = controls.exposureCompensationEv {
            let clamped = min(max(ev, device.minExposureTargetBias), device.maxExposureTargetBias)
            device.setExposureTargetBias(clamped, completionHandler: nil)
        }
        if let focus = controls.focus {
            switch focus {
            case .auto: if device.isFocusModeSupported(.autoFocus) { device.focusMode = .autoFocus }
            case .continuousVideo: if device.isFocusModeSupported(.continuousAutoFocus) { device.focusMode = .continuousAutoFocus }
            case .continuousPicture: if device.isFocusModeSupported(.continuousAutoFocus) { device.focusMode = .continuousAutoFocus }
            case .fixed: if device.isFocusModeSupported(.locked) { device.focusMode = .locked }
            case .manual(let distance):
                if device.isLockingFocusWithCustomLensPositionSupported {
                    device.setFocusModeLockedWithLensPosition(Float(distance), completionHandler: nil)
                }
            }
        }

        return AppliedControls(
            zoom: Float(device.videoZoomFactor),
            torch: device.torchMode == .on,
            focus: controls.focus ?? .continuousVideo,
            exposureCompensationEv: Float(device.exposureTargetBias),
            whiteBalance: controls.whiteBalance ?? .auto,
            flash: controls.flash ?? .off
        )
    }

    // MARK: Stills

    func capture_still(session: NativeHandleId, request: StillCaptureRequest) async throws -> StillPhoto {
        let state = try lookup(session)
        guard let photoOutput = state.photoOutput,
              case .jpeg(let quality) = request.pixelFormat,
              photoOutput.availablePhotoCodecTypes.contains(.jpeg) else {
            throw CameraError.formatUnsupported
        }
        let settings = AVCapturePhotoSettings(format: [
            AVVideoCodecKey: AVVideoCodecType.jpeg,
            AVVideoCompressionPropertiesKey: [AVVideoQualityKey: Double(min(quality, 100)) / 100.0],
        ])
        let flash: AVCaptureDevice.FlashMode
        switch request.flash {
        case .off: flash = .off
        case .on: flash = .on
        case .auto: flash = .auto
        }
        if photoOutput.supportedFlashModes.contains(flash) {
            settings.flashMode = flash
        }

        // AVCapturePhotoOutput holds its delegate weakly.
        let delegate = PhotoDelegate()
        let photo: AVCapturePhoto = try await withCheckedThrowingContinuation { cont in
            delegate.continuation = cont
            photoOutput.capturePhoto(with: settings, delegate: delegate)
        }
        withExtendedLifetime(delegate) {}

        guard let data = photo.fileDataRepresentation() else {
            throw CameraError.backend("photo has no JPEG representation")
        }
        let dims = photo.resolvedSettings.photoDimensions
        return StillPhoto(
            resolution: Resolution(width: UInt32(dims.width), height: UInt32(dims.height)),
            rotationDegrees: 0,
            pixelFormat: request.pixelFormat,
            bytes: data,
            timestampNs: nanoseconds(photo.timestamp)
        )
    }

    // MARK: Internals

    private struct SessionState {
        let captureSession: AVCaptureSession
        let device: AVCaptureDevice
        let output: AVCaptureVideoDataOutput
        let photoOutput: AVCapturePhotoOutput?
        let delegate: SampleDelegate?
        let fanout: FrameFanout
    }

    /// Per-subscriber fan-out of one session's frames. Called from the
    /// sample queue (`yield`), the backend queue (`finish`) and whatever
    /// executor consumes a stream (`onTermination`), hence the lock.
    private final class FrameFanout {
        private struct Subscriber {
            let continuation: AsyncThrowingStream<CameraFrame, Error>.Continuation
            /// Frames lost since the last one this subscriber was handed.
            var dropped: UInt32
        }

        private let lock = NSLock()
        private let depth: Int
        private var subscribers: [UInt64: Subscriber] = [:]
        private var nextId: UInt64 = 0
        private var finished = false

        init(depth: Int) {
            self.depth = depth
        }

        var hasSubscribers: Bool {
            lock.lock()
            defer { lock.unlock() }
            return !subscribers.isEmpty
        }

        func subscribe() -> AsyncThrowingStream<CameraFrame, Error> {
            // `bufferingNewest` drops the oldest buffered frame when the
            // consumer falls behind, matching the Android backend.
            let (stream, continuation) = AsyncThrowingStream<CameraFrame, Error>.makeStream(
                bufferingPolicy: .bufferingNewest(depth)
            )
            lock.lock()
            if finished {
                lock.unlock()
                continuation.finish()
                return stream
            }
            let id = nextId
            nextId &+= 1
            subscribers[id] = Subscriber(continuation: continuation, dropped: 0)
            lock.unlock()
            continuation.onTermination = { [weak self] _ in self?.remove(id) }
            return stream
        }

        func yield(_ frame: CameraFrame) {
            lock.lock()
            defer { lock.unlock() }
            for (id, sub) in subscribers {
                let delivered = CameraFrame(
                    seq: frame.seq,
                    timestampNs: frame.timestampNs,
                    resolution: frame.resolution,
                    rotationDegrees: frame.rotationDegrees,
                    pixelFormat: frame.pixelFormat,
                    planes: frame.planes,
                    bytes: frame.bytes,
                    droppedBefore: sub.dropped
                )
                switch sub.continuation.yield(delivered) {
                case .enqueued:
                    subscribers[id]?.dropped = 0
                case .dropped:
                    // `delivered` is buffered with the old count; an
                    // older buffered frame was evicted for it.
                    subscribers[id]?.dropped = 1
                case .terminated:
                    subscribers.removeValue(forKey: id)
                @unknown default:
                    break
                }
            }
        }

        func finish() {
            lock.lock()
            finished = true
            let current = subscribers.values.map(\.continuation)
            subscribers.removeAll()
            lock.unlock()
            for continuation in current {
                continuation.finish()
            }
        }

        private func remove(_ id: UInt64) {
            lock.lock()
            subscribers.removeValue(forKey: id)
            lock.unlock()
        }
    }

    private final class SampleDelegate: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
        let pixelFormat: PixelFormat
        let fanout: FrameFanout
        /// Only touched on the sample queue.
        private var seq: UInt64 = 0

        init(pixelFormat: PixelFormat, fanout: FrameFanout) {
            self.pixelFormat = pixelFormat
            self.fanout = fanout
        }

        func captureOutput(
            _ output: AVCaptureOutput,
            didOutput sampleBuffer: CMSampleBuffer,
            from connection: AVCaptureConnection
        ) {
            let frameSeq = seq
            seq &+= 1
            // Skip the copy entirely while nobody listens.
            guard fanout.hasSubscribers,
                  let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            let ts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
            let (planes, bytes) = extractPlanes(pixelBuffer)
            fanout.yield(CameraFrame(
                seq: frameSeq,
                timestampNs: nanoseconds(ts),
                resolution: Resolution(
                    width: UInt32(CVPixelBufferGetWidth(pixelBuffer)),
                    height: UInt32(CVPixelBufferGetHeight(pixelBuffer))
                ),
                rotationDegrees: 0,
                pixelFormat: pixelFormat,
                planes: planes,
                bytes: bytes,
                droppedBefore: 0
            ))
        }

        private func extractPlanes(_ buffer: CVPixelBuffer) -> ([FramePlane], Data) {
            CVPixelBufferLockBaseAddress(buffer, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }

            let planeCount = CVPixelBufferGetPlaneCount(buffer)
            if planeCount == 0 {
                let rowStride = CVPixelBufferGetBytesPerRow(buffer)
                let size = rowStride * CVPixelBufferGetHeight(buffer)
                guard let ptr = CVPixelBufferGetBaseAddress(buffer) else { return ([], Data()) }
                var data = Data(bytes: ptr, count: size)
                if case .rgba8 = pixelFormat {
                    // Captured as BGRA (the only packed format the video
                    // data output emits); swap to the requested order.
                    data.withUnsafeMutableBytes { raw in
                        let px = raw.bindMemory(to: UInt8.self)
                        var i = 0
                        while i + 3 < px.count {
                            px.swapAt(i, i + 2)
                            i += 4
                        }
                    }
                }
                let plane = FramePlane(
                    offset: 0,
                    rowStride: UInt32(rowStride),
                    pixelStride: 4,
                    width: UInt32(CVPixelBufferGetWidth(buffer)),
                    height: UInt32(CVPixelBufferGetHeight(buffer))
                )
                return ([plane], data)
            }

            var totalSize = 0
            for i in 0..<planeCount {
                totalSize += CVPixelBufferGetBytesPerRowOfPlane(buffer, i)
                    * CVPixelBufferGetHeightOfPlane(buffer, i)
            }
            var data = Data(count: totalSize)
            var planes: [FramePlane] = []
            var offset = 0
            let swapChroma: Bool = { if case .nv21 = pixelFormat { return true } else { return false } }()
            data.withUnsafeMutableBytes { raw in
                guard let dst = raw.bindMemory(to: UInt8.self).baseAddress else { return }
                for i in 0..<planeCount {
                    let rowStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, i)
                    let height = CVPixelBufferGetHeightOfPlane(buffer, i)
                    let planeSize = rowStride * height
                    guard let src = CVPixelBufferGetBaseAddressOfPlane(buffer, i) else { continue }
                    memcpy(dst.advanced(by: offset), src, planeSize)
                    if i == 1 && swapChroma {
                        // The capture is NV12 (Cb before Cr); NV21 wants
                        // Cr first, so swap every interleaved pair.
                        let chroma = dst.advanced(by: offset)
                        var j = 0
                        while j + 1 < planeSize {
                            let cb = chroma[j]
                            chroma[j] = chroma[j + 1]
                            chroma[j + 1] = cb
                            j += 2
                        }
                    }
                    planes.append(
                        FramePlane(
                            offset: UInt32(offset),
                            rowStride: UInt32(rowStride),
                            pixelStride: i == 0 ? 1 : 2,
                            width: UInt32(CVPixelBufferGetWidthOfPlane(buffer, i)),
                            height: UInt32(height)
                        )
                    )
                    offset += planeSize
                }
            }
            return (planes, data)
        }
    }

    private final class PhotoDelegate: NSObject, AVCapturePhotoCaptureDelegate {
        var continuation: CheckedContinuation<AVCapturePhoto, Error>?

        func photoOutput(
            _ output: AVCapturePhotoOutput,
            didFinishProcessingPhoto photo: AVCapturePhoto,
            error: Error?
        ) {
            guard let continuation else { return }
            self.continuation = nil
            if let error {
                continuation.resume(throwing: CameraError.backend(error.localizedDescription))
            } else {
                continuation.resume(returning: photo)
            }
        }
    }

    /// Capture pixel format for a requested stream format, or `nil` when
    /// the video data output cannot produce it.
    ///
    /// - `yuv420` is delivered as bi-planar NV12.
    /// - `nv21` is captured as NV12 and its chroma pairs are swapped.
    /// - `rgba8` is captured as BGRA and swapped per pixel.
    /// - `jpeg` is a still-only format (see `capture_still`).
    private func pixelFormatType(for format: PixelFormat) -> OSType? {
        switch format {
        case .nv21, .yuv420:
            return kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        case .bgra8, .rgba8:
            return kCVPixelFormatType_32BGRA
        case .jpeg:
            return nil
        }
    }

    private func closestPreset(width: UInt32, height: UInt32) -> AVCaptureSession.Preset {
        switch max(width, height) {
        case 0..<360: return .cif352x288
        case 360..<720: return .vga640x480
        case 720..<1280: return .hd1280x720
        case 1280..<1920: return .hd1920x1080
        default: return .hd4K3840x2160
        }
    }

    /// Request `fps` when the active format supports it; otherwise the
    /// device keeps its default rate, which `currentFrameRate` reports.
    private func setFrameRate(_ fps: UInt32, on device: AVCaptureDevice) {
        let rate = Double(fps)
        let supported = device.activeFormat.videoSupportedFrameRateRanges.contains {
            $0.minFrameRate <= rate && rate <= $0.maxFrameRate
        }
        guard supported, (try? device.lockForConfiguration()) != nil else { return }
        let duration = CMTime(value: 1, timescale: CMTimeScale(fps))
        device.activeVideoMinFrameDuration = duration
        device.activeVideoMaxFrameDuration = duration
        device.unlockForConfiguration()
    }

    private func currentFrameRate(of device: AVCaptureDevice) -> UInt32 {
        let duration = device.activeVideoMinFrameDuration
        guard duration.isValid, duration.value > 0 else { return 0 }
        return UInt32((Double(duration.timescale) / Double(duration.value)).rounded())
    }

    private func apply(initial: CameraControls, to device: AVCaptureDevice) throws {
        guard initial.zoom != nil || initial.torch != nil else { return }
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }
        if let z = initial.zoom {
            device.videoZoomFactor = min(
                max(CGFloat(z), device.minAvailableVideoZoomFactor),
                device.maxAvailableVideoZoomFactor
            )
        }
        if let t = initial.torch, device.hasTorch { device.torchMode = t ? .on : .off }
    }

    private func describe(device: AVCaptureDevice) -> CameraDevice {
        let facing: CameraFacing = {
            switch device.position {
            case .front: return .front
            case .back: return .back
            case .unspecified: return .unknown
            @unknown default: return .unknown
            }
        }()
        let lens: LensKind = {
            switch device.deviceType {
            case .builtInUltraWideCamera: return .ultraWide
            case .builtInTelephotoCamera: return .telephoto
            case .builtInTrueDepthCamera: return .trueDepth
            case .builtInLiDARDepthCamera: return .lidar
            case .builtInDualCamera, .builtInDualWideCamera, .builtInTripleCamera: return .logical
            case .builtInWideAngleCamera: return .wide
            default: return .unknown
            }
        }()
        let dims = CMVideoFormatDescriptionGetDimensions(device.activeFormat.formatDescription)
        let caps = CameraCapabilities(
            minZoom: Float(device.minAvailableVideoZoomFactor),
            maxZoom: Float(device.maxAvailableVideoZoomFactor),
            opticalZoomSteps: device.virtualDeviceSwitchOverVideoZoomFactors.map { Float(truncating: $0) },
            focalLengthsMm: [],
            supportsFlash: device.hasFlash,
            supportsTorch: device.hasTorch,
            supportsAutofocus: device.isFocusModeSupported(.continuousAutoFocus),
            supportsManualFocus: device.isLockingFocusWithCustomLensPositionSupported,
            supportsOis: device.activeFormat.isVideoStabilizationModeSupported(.cinematic),
            supportsEis: device.activeFormat.isVideoStabilizationModeSupported(.standard),
            supportsHdr: device.activeFormat.isVideoHDRSupported,
            supportsDepth: !device.activeFormat.supportedDepthDataFormats.isEmpty,
            supportsRaw: false,
            maxStillResolution: Resolution(width: UInt32(dims.width), height: UInt32(dims.height))
        )
        return CameraDevice(
            id: device.uniqueID,
            label: device.localizedName,
            facing: facing,
            lens: lens,
            physicalSensors: device.constituentDevices.map { $0.uniqueID },
            capabilities: caps,
            supportedFormats: []
        )
    }
}

private func nanoseconds(_ time: CMTime) -> UInt64 {
    let seconds = CMTimeGetSeconds(time)
    guard seconds.isFinite, seconds > 0 else { return 0 }
    return UInt64(seconds * 1_000_000_000)
}
