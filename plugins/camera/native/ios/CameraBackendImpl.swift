import AVFoundation
import CoreMedia
import CoreVideo
import Foundation

/// AVFoundation-backed implementation of `CameraBackend`.
///
/// One `CameraBackendImpl` instance may own several sessions
/// concurrently — iOS A12+ exposes `AVCaptureMultiCamSession`
/// (`isMultiCamSupported` is checked in `concurrent_support`).
///
/// Frames are produced on `AVCaptureVideoDataOutput`'s sample buffer
/// delegate queue and forwarded through a per-session
/// `AsyncStream<CameraFrame>` continuation stored in `sessions`.
final class CameraBackendImpl: NSObject, CameraBackend, HandleReleaser {

    private let config: CameraInitConfig
    private var sessions: [UInt64: SessionState] = [:]
    private let queue = DispatchQueue(label: "istmo.camera.backend", qos: .userInitiated)
    private var sampleQueues: [UInt64: DispatchQueue] = [:]

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
            max_concurrent: supported ? UInt32(4) : UInt32(1),
            approved_pairs: [],
            any_combination: supported
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
        guard let device = AVCaptureDevice(uniqueID: config.device_id) else {
            throw BackendException(CameraError.deviceNotFound(config.device_id))
        }

        let captureSession: AVCaptureSession = AVCaptureMultiCamSession.isMultiCamSupported
            ? AVCaptureMultiCamSession()
            : AVCaptureSession()
        captureSession.beginConfiguration()

        let input = try AVCaptureDeviceInput(device: device)
        guard captureSession.canAddInput(input) else {
            throw BackendException(CameraError.backend("cannot add camera input"))
        }
        captureSession.addInput(input)

        let output = AVCaptureVideoDataOutput()
        output.alwaysDiscardsLateVideoFrames = true
        output.videoSettings = [
            kCVPixelBufferPixelFormatTypeKey as String: pixelFormatType(for: config.pixel_format)
        ]
        guard captureSession.canAddOutput(output) else {
            throw BackendException(CameraError.backend("cannot add video output"))
        }
        captureSession.addOutput(output)

        // Pin resolution preset when requested.
        if let res = config.resolution {
            captureSession.sessionPreset = closestPreset(width: res.width, height: res.height)
        }

        // Target FPS: lockForConfiguration + activeVideoMinFrameDuration.
        if let fps = config.target_fps {
            try? device.lockForConfiguration()
            device.activeVideoMinFrameDuration = CMTime(value: 1, timescale: CMTimeScale(fps))
            device.unlockForConfiguration()
        }

        let handleId = IstmoRuntime.shared.allocHandleId("istmo.camera.session")
        let (stream, continuation) = AsyncStream<CameraFrame>.makeStream(
            bufferingPolicy: .bufferingOldest(Int(max(config.frame_pool_depth, 1)))
        )
        let sampleQueue = DispatchQueue(label: "istmo.camera.samples.\(handleId)")
        sampleQueues[handleId] = sampleQueue

        if config.emit_frames {
            let delegate = SampleDelegate(
                sessionId: handleId,
                pixelFormat: config.pixel_format,
                continuation: continuation
            )
            output.setSampleBufferDelegate(delegate, queue: sampleQueue)
            // Keep the delegate alive for the lifetime of the session.
            captureSession.beginConfiguration()
            captureSession.commitConfiguration()
            let state = SessionState(
                captureSession: captureSession,
                device: device,
                output: output,
                delegate: delegate,
                stream: stream,
                continuation: continuation
            )
            sessions[handleId] = state
        } else {
            let state = SessionState(
                captureSession: captureSession,
                device: device,
                output: output,
                delegate: nil,
                stream: stream,
                continuation: continuation
            )
            sessions[handleId] = state
        }

        captureSession.commitConfiguration()
        captureSession.startRunning()

        try apply(initial: config.initial_controls, to: device)

        let (negW, negH) = resolution(from: output)
        return CameraSession(
            session: handleId,
            negotiated: NegotiatedFormat(
                resolution: Resolution(width: negW, height: negH),
                pixel_format: config.pixel_format,
                fps: config.target_fps ?? 30,
                rotation_degrees: 0
            )
        )
    }

    func close(session: UInt64) async throws {
        guard let state = sessions.removeValue(forKey: session) else { return }
        state.captureSession.stopRunning()
        state.continuation.finish()
        sampleQueues.removeValue(forKey: session)
        IstmoRuntime.shared.forgetHandle(session)
    }

    func frames(session: UInt64) -> AsyncThrowingStream<CameraFrame, Error> {
        guard let state = sessions[session] else {
            return AsyncThrowingStream { $0.finish(throwing: BackendException(CameraError.sessionClosed)) }
        }
        return AsyncThrowingStream { continuation in
            Task {
                for await frame in state.stream {
                    continuation.yield(frame)
                }
                continuation.finish()
            }
        }
    }

    func releaseNativeHandle(handleId: UInt64) {
        Task { try? await close(session: handleId) }
    }

    // MARK: Controls

    func apply_controls(session: UInt64, controls: CameraControls) async throws -> AppliedControls {
        guard let state = sessions[session] else {
            throw BackendException(CameraError.sessionClosed)
        }
        let device = state.device
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }

        if let zoom = controls.zoom { device.videoZoomFactor = CGFloat(zoom) }
        if let torch = controls.torch, device.hasTorch {
            device.torchMode = torch ? .on : .off
        }
        if let ev = controls.exposure_compensation_ev {
            device.setExposureTargetBias(ev, completionHandler: nil)
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
            exposure_compensation_ev: Float(device.exposureTargetBias),
            white_balance: controls.white_balance ?? .auto,
            flash: controls.flash ?? .off
        )
    }

    func capture_still(session: UInt64, request: StillCaptureRequest) async throws -> StillPhoto {
        // Wire an `AVCapturePhotoOutput` into the session on open()
        // and call it from here. Left as an app-side exercise.
        throw BackendException(CameraError.backend("capture_still: wire AVCapturePhotoOutput"))
    }

    // MARK: Internals

    private struct SessionState {
        let captureSession: AVCaptureSession
        let device: AVCaptureDevice
        let output: AVCaptureVideoDataOutput
        let delegate: SampleDelegate?
        let stream: AsyncStream<CameraFrame>
        let continuation: AsyncStream<CameraFrame>.Continuation
    }

    private final class SampleDelegate: NSObject, AVCaptureVideoDataOutputSampleBufferDelegate {
        let sessionId: UInt64
        let pixelFormat: PixelFormat
        let continuation: AsyncStream<CameraFrame>.Continuation
        private var seq: UInt64 = 0

        init(sessionId: UInt64, pixelFormat: PixelFormat, continuation: AsyncStream<CameraFrame>.Continuation) {
            self.sessionId = sessionId
            self.pixelFormat = pixelFormat
            self.continuation = continuation
        }

        func captureOutput(
            _ output: AVCaptureOutput,
            didOutput sampleBuffer: CMSampleBuffer,
            from connection: AVCaptureConnection
        ) {
            guard let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
            let ts = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
            let timestampNs = UInt64(CMTimeGetSeconds(ts) * 1_000_000_000)
            let (planes, bytes) = extractPlanes(pixelBuffer)
            let frame = CameraFrame(
                seq: seq,
                timestamp_ns: timestampNs,
                resolution: Resolution(
                    width: UInt32(CVPixelBufferGetWidth(pixelBuffer)),
                    height: UInt32(CVPixelBufferGetHeight(pixelBuffer))
                ),
                rotation_degrees: 0,
                pixel_format: pixelFormat,
                planes: planes,
                bytes: bytes,
                dropped_before: 0
            )
            seq &+= 1
            _ = continuation.yield(frame)
        }

        private func extractPlanes(_ buffer: CVPixelBuffer) -> ([FramePlane], Data) {
            CVPixelBufferLockBaseAddress(buffer, .readOnly)
            defer { CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }

            let planeCount = CVPixelBufferGetPlaneCount(buffer)
            if planeCount == 0 {
                let size = CVPixelBufferGetBytesPerRow(buffer) * CVPixelBufferGetHeight(buffer)
                let ptr = CVPixelBufferGetBaseAddress(buffer)
                let data = Data(bytes: ptr!, count: size)
                let plane = FramePlane(
                    offset: 0,
                    row_stride: UInt32(CVPixelBufferGetBytesPerRow(buffer)),
                    pixel_stride: 4,
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
            var offset: UInt32 = 0
            data.withUnsafeMutableBytes { raw in
                let dst = raw.bindMemory(to: UInt8.self).baseAddress!
                for i in 0..<planeCount {
                    let rowStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, i)
                    let height = CVPixelBufferGetHeightOfPlane(buffer, i)
                    let planeSize = rowStride * height
                    let src = CVPixelBufferGetBaseAddressOfPlane(buffer, i)!
                    memcpy(dst.advanced(by: Int(offset)), src, planeSize)
                    planes.append(
                        FramePlane(
                            offset: offset,
                            row_stride: UInt32(rowStride),
                            pixel_stride: i == 0 ? 1 : 2,
                            width: UInt32(CVPixelBufferGetWidthOfPlane(buffer, i)),
                            height: UInt32(height)
                        )
                    )
                    offset += UInt32(planeSize)
                }
            }
            return (planes, data)
        }
    }

    private func pixelFormatType(for format: PixelFormat) -> UInt32 {
        switch format {
        case .nv21, .yuv420:
            return kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
        case .bgra8:
            return kCVPixelFormatType_32BGRA
        case .rgba8:
            return kCVPixelFormatType_32RGBA
        case .jpeg:
            // AVCaptureVideoDataOutput doesn't emit JPEG directly —
            // the app should hook an `AVCapturePhotoOutput` for
            // stills; stream path downsamples via a transform.
            return kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
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

    private func apply(initial: CameraControls, to device: AVCaptureDevice) throws {
        guard initial.zoom != nil || initial.torch != nil else { return }
        try device.lockForConfiguration()
        defer { device.unlockForConfiguration() }
        if let z = initial.zoom { device.videoZoomFactor = CGFloat(z) }
        if let t = initial.torch, device.hasTorch { device.torchMode = t ? .on : .off }
    }

    private func resolution(from output: AVCaptureVideoDataOutput) -> (UInt32, UInt32) {
        guard let settings = output.recommendedVideoSettings(forVideoCodecType: .h264, assetWriterOutputFileType: .mov),
              let w = settings[AVVideoWidthKey] as? Int,
              let h = settings[AVVideoHeightKey] as? Int else {
            return (0, 0)
        }
        return (UInt32(w), UInt32(h))
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
        let caps = CameraCapabilities(
            min_zoom: Float(device.minAvailableVideoZoomFactor),
            max_zoom: Float(device.maxAvailableVideoZoomFactor),
            optical_zoom_steps: device.virtualDeviceSwitchOverVideoZoomFactors.map { Float(truncating: $0) },
            focal_lengths_mm: [],
            supports_flash: device.hasFlash,
            supports_torch: device.hasTorch,
            supports_autofocus: device.isFocusModeSupported(.continuousAutoFocus),
            supports_manual_focus: device.isLockingFocusWithCustomLensPositionSupported,
            supports_ois: device.activeFormat.isVideoHDRSupported,
            supports_eis: false, // see AVCaptureConnection.preferredVideoStabilizationMode
            supports_hdr: device.activeFormat.isVideoHDRSupported,
            supports_depth: !device.activeFormat.supportedDepthDataFormats.isEmpty,
            supports_raw: false,
            max_still_resolution: Resolution(
                width: UInt32(CMVideoFormatDescriptionGetDimensions(device.activeFormat.formatDescription).width),
                height: UInt32(CMVideoFormatDescriptionGetDimensions(device.activeFormat.formatDescription).height)
            )
        )
        return CameraDevice(
            id: device.uniqueID,
            label: device.localizedName,
            facing: facing,
            lens: lens,
            physical_sensors: device.constituentDevices.map { $0.uniqueID },
            capabilities: caps,
            supported_formats: []
        )
    }
}
