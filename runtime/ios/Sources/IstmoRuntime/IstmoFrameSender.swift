import Foundation
import IstmoFFI

/// What a frame sink did with a submitted frame; mirrors Rust's
/// `istmo_core::native_frame::SubmitStatus` (a test keeps the codes in
/// sync).
///
/// Unless the status `isTaken`, Rust kept nothing (a retained object it
/// was handed is already released).
public enum IstmoSubmitStatus: Int32, Sendable {
    /// Queued.
    case accepted = 0
    /// Queued after evicting an older frame.
    case displaced = 1
    /// The sink ended (consumer gone, closed, shutdown): stop producing.
    case closed = 2
    /// No such sink in this process (stale, or opened elsewhere).
    case unknownSink = 3
    /// The metadata does not decode as the sink's metadata type.
    case badMeta = 4
    /// The platform cannot do this.
    case unsupported = 5
    /// The Rust runtime is not running.
    case notStarted = 6
    /// A malformed argument, or a sender already closed.
    case invalid = 7
    /// The sink is full and refuses new frames.
    case full = 8
    /// Rust failed internally after taking the buffer (a bug); Rust releases it.
    case `internal` = 9
    /// The sink carries another metadata type than this sender's.
    case typeMismatch = 10

    /// Whether the sink kept the frame.
    public var isTaken: Bool { self == .accepted || self == .displaced }

    /// Whether every later submit fails the same way: stop producing.
    public var shouldStop: Bool {
        self == .closed || self == .unknownSink || self == .notStarted || self == .typeMismatch
    }

    init(code: Int32) {
        self = IstmoSubmitStatus(rawValue: code) ?? .internal
    }
}

/// What a retained object handed to `submitRetained` is.
public enum IstmoRetainedKind: UInt32, Sendable {
    case other = 0
    case cvPixelBuffer = 1
    case ioSurface = 2
    case cmSampleBuffer = 3
}

/// The producer end of a Rust frame sink, handed to a backend as the
/// `FrameSinkId<M>` argument of a plugin method. One sender is one
/// producer session: the sink ends when it is closed (`close()`,
/// `close(error:)`) or deinitialised. Frames go straight to the Rust
/// consumer, metadata wire-encoded, buffers never copied (except
/// `submitBytes`). Platform conveniences (`submit(_: CVPixelBuffer, meta:)`,
/// `submit(_: IOSurfaceRef, meta:)`, `submit(_: CMSampleBuffer, meta:)`)
/// live in `IstmoFrameSender+Apple.swift`.
///
/// Thread-safe: submits are serialised, so the sender can be captured by
/// capture callbacks on any queue.
public final class IstmoFrameSender<M>: @unchecked Sendable {
    /// The sink's id, as Rust allocated it.
    public let sinkId: UInt64
    private let encode: (M) -> Data
    private let lock = NSLock()
    private var handle: OpaquePointer?
    private let openStatus: IstmoSubmitStatus
    private var ended = false

    private init(sinkId: UInt64, fingerprint: UInt64, encode: @escaping (M) -> Data) {
        self.sinkId = sinkId
        self.encode = encode
        var status: Int32 = 0
        handle = istmo_ios_open_frame_producer(sinkId, fingerprint, &status)
        openStatus = handle == nil ? IstmoSubmitStatus(code: status) : .accepted
    }

    /// Open a sender on sink `sinkId` whose metadata type `fingerprint`
    /// names. Generated dispatchers call this; backends receive senders.
    public static func open(sinkId: UInt64, fingerprint: UInt64, encode: @escaping (M) -> Data) -> IstmoFrameSender<M> {
        IstmoFrameSender(sinkId: sinkId, fingerprint: fingerprint, encode: encode)
    }

    deinit {
        if let handle {
            if !ended { istmo_ios_end_frame_producer(handle, nil, 0) }
            istmo_ios_free_frame_producer(handle)
        }
    }

    /// Why the sender cannot submit at all, if it cannot.
    public var failure: IstmoSubmitStatus? {
        lock.withLock { handle == nil && !ended ? openStatus : nil }
    }

    private func withHandle(_ body: (OpaquePointer) -> Int32) -> IstmoSubmitStatus {
        lock.withLock {
            if ended { return .closed }
            guard let handle else { return openStatus }
            return IstmoSubmitStatus(code: body(handle))
        }
    }

    /// Hand over a retained object: Rust owns the +1 reference in
    /// `object` from here on and gives it up through `release` (on any
    /// thread) once the frame drops, or at once when the sink refuses it.
    public func submitRetained(
        _ object: UnsafeMutableRawPointer,
        kind: IstmoRetainedKind,
        release: @escaping @convention(c) (UnsafeMutableRawPointer?) -> Void,
        meta: M
    ) -> IstmoSubmitStatus {
        let bytes = encode(meta)
        var reached = false
        let status = withHandle { handle in
            reached = true
            return bytes.withUnsafeBytes { raw in
                istmo_ios_submit_native_buffer(
                    handle, kind.rawValue, object, release,
                    raw.bindMemory(to: UInt8.self).baseAddress, bytes.count)
            }
        }
        // Rust never saw the object: give the reference back here.
        if !reached { release(object) }
        return status
    }

    /// Copy `data` into a frame (one copy).
    public func submitBytes(_ data: Data, meta: M) -> IstmoSubmitStatus {
        let bytes = encode(meta)
        return withHandle { handle in
            data.withUnsafeBytes { payload in
                bytes.withUnsafeBytes { raw in
                    istmo_ios_submit_frame_bytes(
                        handle,
                        payload.bindMemory(to: UInt8.self).baseAddress, data.count,
                        raw.bindMemory(to: UInt8.self).baseAddress, bytes.count)
                }
            }
        }
    }

    /// Whether the sender was closed or never reached its sink.
    public var isClosed: Bool { lock.withLock { ended || handle == nil } }

    /// End the stream: the consumer receives what is queued, then the end.
    public func close() { end(nil) }

    /// End the stream with an error the consumer reads as the end reason.
    public func close(error: String) { end(error) }

    private func end(_ error: String?) {
        lock.withLock {
            guard !ended else { return }
            ended = true
            guard let handle else { return }
            if let error {
                let bytes = Array(error.utf8)
                bytes.withUnsafeBufferPointer { istmo_ios_end_frame_producer(handle, $0.baseAddress, bytes.count) }
            } else {
                istmo_ios_end_frame_producer(handle, nil, 0)
            }
        }
    }
}

enum IstmoFrames {
    /// The native entry version this runtime was built against.
    static let abiVersion: UInt32 = 1

    /// The library's entry version, when it differs from ours.
    static func abiMismatch() -> UInt32? {
        let found = istmo_abi_version()
        return found == abiVersion ? nil : found
    }
}
