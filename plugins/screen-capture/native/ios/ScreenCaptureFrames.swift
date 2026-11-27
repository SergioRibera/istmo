import CoreGraphics
import CoreMedia
import CoreVideo
import Foundation
import IOSurface
import IstmoRuntime

// Frames and chunks as the plugin hands them over: metadata from sample
// buffers, the pool rule (never the last free buffer), PCM conversion,
// raw pixels for `screenshot_pixels`, clocks and event fan-out.

/// Pixel formats as Core Video codes.
enum ScreenCaptureFourcc {
    static let bgra: OSType = kCVPixelFormatType_32BGRA
    static let yuv420Video: OSType = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange
    static let yuv420Full: OSType = kCVPixelFormatType_420YpCbCr8BiPlanarFullRange

    /// The plugin's format and range a Core Video code stands for.
    static func format(_ code: OSType) -> (ScreenCapture.PixelFormat, ScreenCapture.ColourRange)? {
        switch code {
        case bgra: return (.bgra8, .full)
        case yuv420Video: return (.nv12, .limited)
        case yuv420Full: return (.nv12, .full)
        default: return nil
        }
    }

    /// The code asked of ScreenCaptureKit for `format`.
    static func code(for format: ScreenCapture.PixelFormat) -> OSType? {
        switch format {
        case .bgra8: return bgra
        case .nv12: return yuv420Video
        case .bgrx8, .rgba8, .rgbx8, .xr30, .xb30, .ar30, .rgba16f, .p010: return nil
        }
    }

    static func colour(_ format: ScreenCapture.PixelFormat, _ range: ScreenCapture.ColourRange) -> ScreenCapture.Colour {
        switch format {
        case .nv12: return ScreenCapture.Colour(space: .bt709, transfer: .bt709, range: range)
        case .bgra8, .bgrx8, .rgba8, .rgbx8, .xr30, .xb30, .ar30, .rgba16f, .p010:
            return ScreenCapture.Colour(space: .srgb, transfer: .srgb, range: .full)
        }
    }
}

/// Clocks: sample buffers are stamped on the host time clock
/// (`mach_absolute_time`); `wallUs` is UNIX-epoch µs per frame,
/// `wall_now − (host_now − pts)`.
enum ScreenCaptureClock {
    static func nanoseconds(_ time: CMTime) -> UInt64? {
        guard time.isValid, time.timescale > 0, time.value >= 0 else { return nil }
        let ns = time.value.multipliedReportingOverflow(by: 1_000_000_000)
        if ns.overflow {
            return UInt64(time.value / Int64(time.timescale)) * 1_000_000_000
        }
        return UInt64(ns.partialValue / Int64(time.timescale))
    }

    static func hostNow() -> UInt64 {
        nanoseconds(CMClockGetTime(CMClockGetHostTimeClock())) ?? 0
    }

    static func wallNowUs() -> UInt64 {
        let seconds = Date().timeIntervalSince1970
        return seconds > 0 ? UInt64(seconds * 1_000_000) : 0
    }

    /// The UNIX-epoch time of an instant `ptsNs` on the host clock.
    static func wallUs(forHostNs ptsNs: UInt64) -> UInt64 {
        let now = hostNow()
        let ageUs = now > ptsNs ? (now - ptsNs) / 1000 : 0
        let wall = wallNowUs()
        return wall > ageUs ? wall - ageUs : 0
    }
}

/// What a frame's attachments say (ScreenCaptureKit's frame info, or
/// ReplayKit's orientation).
struct ScreenCaptureFrameInfo {
    /// `SCFrameStatus` raw value; `nil` when the producer reports none.
    var status: Int?
    /// Points in the surface.
    var contentRect: CGRect?
    /// Pixels per point.
    var scaleFactor: Double?
    /// `SCStreamFrameInfo.contentScale`: the content's scale relative to
    /// the output.
    var contentScale: Double?
    /// Pixels; `nil` = not reported (all may have changed), empty = none.
    var dirtyRects: [CGRect]?
    /// `CGImagePropertyOrientation` raw value.
    var orientation: UInt32?

    /// The valid area of a `size` buffer: points times
    /// `scaleFactor / contentScale` (as macOS; unconfirmed on a device).
    func contentRect(in size: ScreenCapture.Size) -> ScreenCapture.Rect {
        let full = ScreenCapture.Rect(x: 0, y: 0, width: size.width, height: size.height)
        guard let rect = contentRect else { return full }
        var contentScale = self.contentScale ?? 1
        if !contentScale.isFinite || contentScale <= 0 { contentScale = 1 }
        let scale = (scaleFactor ?? 1) / contentScale
        guard let covered = Self.covering(rect, scale: scale) else { return full }
        return Self.intersect(covered, full) ?? full
    }

    func damage(in size: ScreenCapture.Size) -> ScreenCapture.Damage {
        guard let rects = dirtyRects else { return .full }
        if rects.isEmpty { return .unchanged }
        let full = ScreenCapture.Rect(x: 0, y: 0, width: size.width, height: size.height)
        var kept: [ScreenCapture.Rect] = []
        for rect in rects {
            guard let covered = Self.covering(rect, scale: 1), let inside = Self.intersect(covered, full) else { continue }
            if kept.count == 32 { return .full }
            kept.append(inside)
        }
        return kept.isEmpty ? .full : .rects(kept)
    }

    var scalePermille: UInt32 {
        guard let scale = scaleFactor, scale.isFinite, scale > 0 else { return 1000 }
        return UInt32(min((scale * 1000).rounded(), Double(UInt32.max)))
    }

    /// What is still to apply to show the frame upright: EXIF orientation
    /// values (1 up … 8 left) as mirror-then-clockwise-turns.
    var pluginOrientation: ScreenCapture.Orientation {
        switch orientation ?? 1 {
        case 2: return ScreenCapture.Orientation(mirrored: true, rotation: .r0)
        case 3: return ScreenCapture.Orientation(mirrored: false, rotation: .r180)
        case 4: return ScreenCapture.Orientation(mirrored: true, rotation: .r180)
        case 5: return ScreenCapture.Orientation(mirrored: true, rotation: .r270)
        case 6: return ScreenCapture.Orientation(mirrored: false, rotation: .r90)
        case 7: return ScreenCapture.Orientation(mirrored: true, rotation: .r90)
        case 8: return ScreenCapture.Orientation(mirrored: false, rotation: .r270)
        default: return ScreenCapture.Orientation(mirrored: false, rotation: .r0)
        }
    }

    static func covering(_ rect: CGRect, scale: Double) -> ScreenCapture.Rect? {
        let x = Double(rect.minX), y = Double(rect.minY)
        let width = Double(rect.width), height = Double(rect.height)
        guard [x, y, width, height, scale].allSatisfy({ $0.isFinite }), width > 0, height > 0 else { return nil }
        let left = (x * scale).rounded(.down), top = (y * scale).rounded(.down)
        let right = ((x + width) * scale).rounded(.up), bottom = ((y + height) * scale).rounded(.up)
        let limit = Double(Int32.max)
        guard abs(left) < limit, abs(top) < limit, right - left < limit, bottom - top < limit else { return nil }
        return ScreenCapture.Rect(x: Int32(left), y: Int32(top), width: UInt32(right - left), height: UInt32(bottom - top))
    }

    static func intersect(_ a: ScreenCapture.Rect, _ b: ScreenCapture.Rect) -> ScreenCapture.Rect? {
        let left = max(Int64(a.x), Int64(b.x)), top = max(Int64(a.y), Int64(b.y))
        let right = min(Int64(a.x) + Int64(a.width), Int64(b.x) + Int64(b.width))
        let bottom = min(Int64(a.y) + Int64(a.height), Int64(b.y) + Int64(b.height))
        guard right > left, bottom > top else { return nil }
        return ScreenCapture.Rect(x: Int32(left), y: Int32(top), width: UInt32(right - left), height: UInt32(bottom - top))
    }
}

/// The pixel buffer of a video sample and its layout.
struct ScreenCapturePixels {
    let buffer: CVPixelBuffer
    let size: ScreenCapture.Size
    let code: OSType

    init?(_ sample: CMSampleBuffer) {
        guard CMSampleBufferIsValid(sample), let buffer = CMSampleBufferGetImageBuffer(sample) else { return nil }
        self.buffer = buffer
        size = ScreenCapture.Size(
            width: UInt32(clamping: CVPixelBufferGetWidth(buffer)),
            height: UInt32(clamping: CVPixelBufferGetHeight(buffer)))
        code = CVPixelBufferGetPixelFormatType(buffer)
    }

    var surface: IOSurfaceRef? {
        CVPixelBufferGetIOSurface(buffer)?.takeUnretainedValue()
    }

    /// Plane offsets (from the `IOSurface`'s base address), strides and
    /// sizes; from the pixel buffer alone when it has no surface.
    var planes: [ScreenCapture.PlaneLayout] {
        func plane(_ offset: Int, _ stride: Int, _ height: Int) -> ScreenCapture.PlaneLayout {
            ScreenCapture.PlaneLayout(
                offset: UInt32(clamping: offset), stride: UInt32(clamping: stride),
                size: UInt64(clamping: stride) * UInt64(clamping: height))
        }
        guard let surface else {
            return [plane(0, CVPixelBufferGetBytesPerRow(buffer), CVPixelBufferGetHeight(buffer))]
        }
        let count = IOSurfaceGetPlaneCount(surface)
        if count == 0 {
            return [plane(0, IOSurfaceGetBytesPerRow(surface), IOSurfaceGetHeight(surface))]
        }
        let base = Int(bitPattern: IOSurfaceGetBaseAddress(surface))
        return (0..<count).map { index in
            let start = Int(bitPattern: IOSurfaceGetBaseAddressOfPlane(surface, index))
            return plane(max(0, start - base), IOSurfaceGetBytesPerRowOfPlane(surface, index),
                         IOSurfaceGetHeightOfPlane(surface, index))
        }
    }

    /// The whole buffer copied out, planes back to back in the layout
    /// `planes` reports (`CopyStage.exhaustionReadback`).
    func copyOut() -> (Data, [ScreenCapture.PlaneLayout])? {
        guard CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess else { return nil }
        defer { _ = CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        var bytes: [UInt8] = []
        var layout: [ScreenCapture.PlaneLayout] = []
        let planar = CVPixelBufferIsPlanar(buffer)
        let count = planar ? CVPixelBufferGetPlaneCount(buffer) : 1
        for index in 0..<count {
            let base = planar ? CVPixelBufferGetBaseAddressOfPlane(buffer, index) : CVPixelBufferGetBaseAddress(buffer)
            let stride = planar ? CVPixelBufferGetBytesPerRowOfPlane(buffer, index) : CVPixelBufferGetBytesPerRow(buffer)
            let height = planar ? CVPixelBufferGetHeightOfPlane(buffer, index) : CVPixelBufferGetHeight(buffer)
            guard let base else { return nil }
            layout.append(ScreenCapture.PlaneLayout(
                offset: UInt32(clamping: bytes.count), stride: UInt32(clamping: stride),
                size: UInt64(clamping: stride * height)))
            bytes.append(contentsOf: UnsafeRawBufferPointer(start: base, count: stride * height))
        }
        return (Data(bytes), layout)
    }

    /// Raw pixels: cropped to `crop`, turned upright with `orientation`
    /// when `layout.upright`, in the asked byte order. BGRA is read as is;
    /// NV12 is converted (BT.709 in its range). `nil` for other formats.
    func raw(crop: ScreenCapture.Rect, orientation: ScreenCapture.Orientation,
             layout: ScreenCapture.PixelLayout) -> (width: UInt32, height: UInt32, bytes: [UInt8])? {
        guard layout.format != .rgba16f else { return nil }
        guard CVPixelBufferLockBaseAddress(buffer, .readOnly) == kCVReturnSuccess else { return nil }
        defer { _ = CVPixelBufferUnlockBaseAddress(buffer, .readOnly) }
        let width = Int(crop.width), height = Int(crop.height)
        let x0 = Int(crop.x), y0 = Int(crop.y)
        guard width > 0, height > 0, x0 >= 0, y0 >= 0,
              x0 + width <= Int(size.width), y0 + height <= Int(size.height) else { return nil }
        var rgba = [UInt8](repeating: 0, count: width * height * 4)
        switch ScreenCaptureFourcc.format(code)?.0 {
        case .bgra8?:
            guard let base = CVPixelBufferGetBaseAddress(buffer) else { return nil }
            let stride = CVPixelBufferGetBytesPerRow(buffer)
            let src = base.assumingMemoryBound(to: UInt8.self)
            for y in 0..<height {
                for x in 0..<width {
                    let s = (y0 + y) * stride + (x0 + x) * 4, d = (y * width + x) * 4
                    rgba[d] = src[s + 2]; rgba[d + 1] = src[s + 1]; rgba[d + 2] = src[s]; rgba[d + 3] = src[s + 3]
                }
            }
        case .nv12?:
            guard let lumaBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 0),
                  let chromaBase = CVPixelBufferGetBaseAddressOfPlane(buffer, 1) else { return nil }
            let full = code == ScreenCaptureFourcc.yuv420Full
            let lumaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 0)
            let chromaStride = CVPixelBufferGetBytesPerRowOfPlane(buffer, 1)
            let luma = lumaBase.assumingMemoryBound(to: UInt8.self)
            let chroma = chromaBase.assumingMemoryBound(to: UInt8.self)
            for y in 0..<height {
                for x in 0..<width {
                    let px = x0 + x, py = y0 + y
                    let c = (py / 2) * chromaStride + (px / 2) * 2
                    let (r, g, b) = Self.rgb(y: luma[py * lumaStride + px], cb: chroma[c], cr: chroma[c + 1], full: full)
                    let d = (y * width + x) * 4
                    rgba[d] = r; rgba[d + 1] = g; rgba[d + 2] = b; rgba[d + 3] = 255
                }
            }
        default:
            return nil
        }
        var outWidth = width, outHeight = height
        if layout.upright {
            (rgba, outWidth, outHeight) = Self.upright(rgba, width: width, height: height, orientation: orientation)
        }
        if layout.format == .bgra8 {
            for i in Swift.stride(from: 0, to: rgba.count, by: 4) { rgba.swapAt(i, i + 2) }
        }
        return (UInt32(outWidth), UInt32(outHeight), rgba)
    }

    /// BT.709 YCbCr → RGB.
    static func rgb(y: UInt8, cb: UInt8, cr: UInt8, full: Bool) -> (UInt8, UInt8, UInt8) {
        let luma = full ? Double(y) : (Double(y) - 16) * 255 / 219
        let scale = full ? 1.0 : 255.0 / 224
        let u = (Double(cb) - 128) * scale, v = (Double(cr) - 128) * scale
        func clamp(_ value: Double) -> UInt8 { UInt8(max(0, min(255, value.rounded()))) }
        return (clamp(luma + 1.5748 * v), clamp(luma - 0.1873 * u - 0.4681 * v), clamp(luma + 1.8556 * u))
    }

    /// Mirror (horizontally), then turn clockwise by quarter turns.
    static func upright(_ pixels: [UInt8], width: Int, height: Int,
                        orientation: ScreenCapture.Orientation) -> ([UInt8], Int, Int) {
        var src = pixels
        if orientation.mirrored {
            for y in 0..<height {
                for x in 0..<(width / 2) {
                    for k in 0..<4 { src.swapAt((y * width + x) * 4 + k, (y * width + width - 1 - x) * 4 + k) }
                }
            }
        }
        let turns: Int
        switch orientation.rotation {
        case .r0: return (src, width, height)
        case .r90: turns = 1
        case .r180: turns = 2
        case .r270: turns = 3
        }
        let outWidth = turns == 2 ? width : height, outHeight = turns == 2 ? height : width
        var out = [UInt8](repeating: 0, count: src.count)
        for y in 0..<height {
            for x in 0..<width {
                let (dx, dy): (Int, Int)
                switch turns {
                case 1: (dx, dy) = (height - 1 - y, x)
                case 2: (dx, dy) = (width - 1 - x, height - 1 - y)
                default: (dx, dy) = (y, width - 1 - x)
                }
                for k in 0..<4 { out[(dy * outWidth + dx) * 4 + k] = src[(y * width + x) * 4 + k] }
            }
        }
        return (out, outWidth, outHeight)
    }
}

/// PCM out of an audio sample buffer, interleaved in the asked format.
enum ScreenCapturePcm {
    struct Chunk {
        let bytes: Data
        let frames: UInt32
        let rate: UInt32
        let channels: UInt8
    }

    static func chunk(_ sample: CMSampleBuffer, format: ScreenCapture.SampleFormat) -> Chunk? {
        guard CMSampleBufferIsValid(sample), CMSampleBufferDataIsReady(sample),
              let description = CMSampleBufferGetFormatDescription(sample),
              let asbd = CMAudioFormatDescriptionGetStreamBasicDescription(description)?.pointee,
              asbd.mFormatID == kAudioFormatLinearPCM,
              let block = CMSampleBufferGetDataBuffer(sample) else { return nil }
        let length = CMBlockBufferGetDataLength(block)
        guard length > 0 else { return nil }
        var raw = [UInt8](repeating: 0, count: length)
        let copied = raw.withUnsafeMutableBytes { dest in
            CMBlockBufferCopyDataBytes(block, atOffset: 0, dataLength: length, destination: dest.baseAddress!)
        }
        guard copied == 0 else { return nil }
        let channels = Int(asbd.mChannelsPerFrame)
        let frames = CMSampleBufferGetNumSamples(sample)
        guard channels > 0, frames > 0 else { return nil }
        guard let samples = decode(raw, asbd: asbd, frames: frames) else { return nil }
        return Chunk(bytes: encode(samples, format: format), frames: UInt32(clamping: frames),
                     rate: UInt32(max(0, asbd.mSampleRate.rounded())), channels: UInt8(clamping: channels))
    }

    /// Interleaved `Float` samples from linear PCM in any of the layouts
    /// ReplayKit and ScreenCaptureKit deliver: float32 or signed 16 / 32
    /// bit, either endianness, interleaved or planar (planes back to back
    /// in the block buffer).
    static func decode(_ raw: [UInt8], asbd: AudioStreamBasicDescription, frames: Int) -> [Float]? {
        let channels = Int(asbd.mChannelsPerFrame)
        let bytes = Int(asbd.mBitsPerChannel) / 8
        let float = asbd.mFormatFlags & kAudioFormatFlagIsFloat != 0
        let bigEndian = asbd.mFormatFlags & kAudioFormatFlagIsBigEndian != 0
        let planar = asbd.mFormatFlags & kAudioFormatFlagIsNonInterleaved != 0
        guard [2, 4].contains(bytes), !(float && bytes != 4), raw.count >= frames * channels * bytes else { return nil }
        func word(_ offset: Int) -> UInt32 {
            var value: UInt32 = 0
            for k in 0..<bytes {
                let byte = UInt32(raw[offset + (bigEndian ? bytes - 1 - k : k)])
                value |= byte << (8 * UInt32(k))
            }
            return value
        }
        func sample(_ offset: Int) -> Float {
            let value = word(offset)
            if float { return Float(bitPattern: value) }
            if bytes == 2 { return Float(Int16(bitPattern: UInt16(truncatingIfNeeded: value))) / 32_768 }
            return Float(Int32(bitPattern: value)) / 2_147_483_648
        }
        var out = [Float](repeating: 0, count: frames * channels)
        for frame in 0..<frames {
            for channel in 0..<channels {
                let index = planar ? (channel * frames + frame) : (frame * channels + channel)
                out[frame * channels + channel] = sample(index * bytes)
            }
        }
        return out
    }

    static func encode(_ samples: [Float], format: ScreenCapture.SampleFormat) -> Data {
        var bytes: [UInt8] = []
        bytes.reserveCapacity(samples.count * (format == .f32 ? 4 : 2))
        for sample in samples {
            switch format {
            case .f32:
                let bits = sample.bitPattern
                bytes.append(contentsOf: [UInt8(bits & 0xFF), UInt8((bits >> 8) & 0xFF),
                                          UInt8((bits >> 16) & 0xFF), UInt8(bits >> 24)])
            case .s16:
                let clamped = sample.isFinite ? max(-1, min(1, sample)) : 0
                let value = UInt16(bitPattern: Int16((clamped * 32_767).rounded()))
                bytes.append(contentsOf: [UInt8(value & 0xFF), UInt8(value >> 8)])
            }
        }
        return Data(bytes)
    }
}

/// Holds on frames the consumer has not dropped yet, so the producer is
/// never left without a buffer: a frame is handed out only while fewer
/// than `depth - 1` are out. Releases come from Rust, on any thread.
final class ScreenCaptureLedger: @unchecked Sendable {
    private let lock = NSLock()
    private var poolDepth: Int
    private var inFlight = 0
    private var exhausted = false
    private var episodes: UInt32 = 0

    init(depth: Int) { poolDepth = depth }

    /// The pool depth holds are counted against.
    var depth: Int { lock.withLock { poolDepth } }

    /// The producer showed `surfaces` distinct buffers: its pool is at
    /// least that deep (nothing documents it equals `queueDepth`).
    func grow(to surfaces: Int) { lock.withLock { poolDepth = max(poolDepth, surfaces) } }

    /// `nil` when a frame may go out (and is now counted), else the
    /// exhaustion episode and whether it just began.
    func hold() -> (episode: UInt32, isNew: Bool)? {
        lock.withLock {
            if inFlight < poolDepth - 1 {
                inFlight += 1
                exhausted = false
                return nil
            }
            let isNew = !exhausted
            if isNew { exhausted = true; episodes += 1 }
            return (episodes, isNew)
        }
    }

    func release() { lock.withLock { inFlight = max(0, inFlight - 1) } }
}

/// Which ledger each submitted object belongs to: the release function
/// Rust calls is a C function, so it finds its ledger here.
final class ScreenCaptureLedgers: @unchecked Sendable {
    static let shared = ScreenCaptureLedgers()
    private let lock = NSLock()
    private var owners: [UInt: [ScreenCaptureLedger]] = [:]

    func track(_ object: UnsafeMutableRawPointer, _ ledger: ScreenCaptureLedger) {
        lock.withLock { owners[UInt(bitPattern: object), default: []].append(ledger) }
    }

    func untrack(_ object: UnsafeMutableRawPointer) -> ScreenCaptureLedger? {
        lock.withLock {
            let key = UInt(bitPattern: object)
            guard var ledgers = owners[key], !ledgers.isEmpty else { return nil }
            let ledger = ledgers.removeLast()
            owners[key] = ledgers.isEmpty ? nil : ledgers
            return ledger
        }
    }

    /// Hand `object` to `sender` without a copy; `ledger` counts it until
    /// Rust drops the frame.
    static func submit<M>(_ object: AnyObject, kind: IstmoRetainedKind, meta: M,
                          ledger: ScreenCaptureLedger, to sender: IstmoFrameSender<M>) -> IstmoSubmitStatus {
        let pointer = Unmanaged.passRetained(object).toOpaque()
        shared.track(pointer, ledger)
        return sender.submitRetained(pointer, kind: kind, release: { object in
            guard let object else { return }
            ScreenCaptureLedgers.shared.untrack(object)?.release()
            Unmanaged<AnyObject>.fromOpaque(object).release()
        }, meta: meta)
    }
}

/// A session's events: every subscriber first gets `started` and the
/// newest events so far, then live ones.
final class ScreenCaptureEventHub<Event>: @unchecked Sendable {
    private let lock = NSLock()
    private var backlog: [Event] = []
    private var subscribers: [SubscriberKey: AsyncThrowingStream<Event, Error>.Continuation] = [:]
    private var ended = false
    private var nextKey = 0

    struct SubscriberKey: Hashable { let value: Int }

    func emit(_ event: Event) {
        let live: [AsyncThrowingStream<Event, Error>.Continuation] = lock.withLock {
            guard !ended else { return [] }
            backlog.append(event)
            if backlog.count > 64 { backlog.removeFirst(backlog.count - 64) }
            return Array(subscribers.values)
        }
        for continuation in live { continuation.yield(event) }
    }

    /// `first` is replayed before the backlog (the stream as negotiated).
    func subscribe(first: Event?) -> AsyncThrowingStream<Event, Error> {
        let (stream, continuation) = AsyncThrowingStream<Event, Error>.makeStream(
            bufferingPolicy: .bufferingNewest(64))
        let (replay, isEnded, key): ([Event], Bool, SubscriberKey) = lock.withLock {
            let key = SubscriberKey(value: nextKey)
            nextKey += 1
            if !ended { subscribers[key] = continuation }
            return (backlog, ended, key)
        }
        if let first { continuation.yield(first) }
        for event in replay { continuation.yield(event) }
        if isEnded {
            continuation.finish()
        } else {
            continuation.onTermination = { [weak self] _ in self?.remove(key) }
        }
        return stream
    }

    private func remove(_ key: SubscriberKey) {
        lock.withLock { _ = subscribers.removeValue(forKey: key) }
    }

    func end() {
        let live: [AsyncThrowingStream<Event, Error>.Continuation] = lock.withLock {
            ended = true
            defer { subscribers.removeAll() }
            return Array(subscribers.values)
        }
        for continuation in live { continuation.finish() }
    }
}
