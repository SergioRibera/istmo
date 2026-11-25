import Foundation
import IstmoFFI

/// What a frame sink did with a submitted frame; mirrors Rust's
/// `istmo_core::native_frame::SubmitStatus`.
///
/// Unless the status `isTaken`, Rust kept nothing (a retained object it
/// was handed is already released).
public enum IstmoSubmitStatus: Int32 {
    /// Queued.
    case accepted = 0
    /// Queued after evicting an older frame.
    case displaced = 1
    /// The consumer is gone or the sink was closed: stop producing.
    case closed = 2
    /// No such sink in this process (stale, or opened elsewhere).
    case unknownSink = 3
    /// The metadata does not decode as the sink's metadata type.
    case badMeta = 4
    /// The platform cannot do this.
    case unsupported = 5
    /// The Rust runtime is not running.
    case notStarted = 6
    /// A malformed argument.
    case invalid = 7
    /// The sink is full and refuses new frames.
    case full = 8

    /// Whether the sink kept the frame.
    public var isTaken: Bool { self == .accepted || self == .displaced }

    init(code: Int32) {
        self = IstmoSubmitStatus(rawValue: code) ?? .invalid
    }
}

/// What a retained object handed to `submitRetained` is.
public enum IstmoRetainedKind: UInt32 {
    case other = 0
    case cvPixelBuffer = 1
    case ioSurface = 2
    case cmSampleBuffer = 3
}

/// A Rust frame sink a backend feeds: frames go straight to the Rust
/// consumer, metadata wire-encoded, buffers never copied (except
/// `submitBytes`).
///
/// Generated dispatchers build it from the `FrameSinkId<M>` argument of a
/// plugin method; backends never construct one. Platform conveniences
/// (`submit(_: CVPixelBuffer, meta:)`, `submit(_: IOSurfaceRef, meta:)`)
/// live in `IstmoFrameSink+Apple.swift`.
public struct IstmoFrameSink<M> {
    /// The sink's id, as Rust allocated it.
    public let id: UInt64
    private let encode: (M) -> Data

    public init(id: UInt64, encode: @escaping (M) -> Data) {
        self.id = id
        self.encode = encode
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
        let code = bytes.withUnsafeBytes { raw in
            istmo_ios_submit_native_buffer(
                id, kind.rawValue, object, release,
                raw.bindMemory(to: UInt8.self).baseAddress, bytes.count)
        }
        return IstmoSubmitStatus(code: code)
    }

    /// Copy `data` into a frame (one copy).
    public func submitBytes(_ data: Data, meta: M) -> IstmoSubmitStatus {
        let bytes = encode(meta)
        let code = data.withUnsafeBytes { payload in
            bytes.withUnsafeBytes { raw in
                istmo_ios_submit_frame_bytes(
                    id,
                    payload.bindMemory(to: UInt8.self).baseAddress, data.count,
                    raw.bindMemory(to: UInt8.self).baseAddress, bytes.count)
            }
        }
        return IstmoSubmitStatus(code: code)
    }

    /// End the stream: the consumer receives what is queued, then the end.
    @discardableResult
    public func close() -> Bool {
        istmo_ios_close_frame_sink(id)
    }
}
