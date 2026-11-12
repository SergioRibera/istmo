// Minimal stand-ins for the Foundation / Dispatch API surface the runtime
// uses, so the sources can be type-checked on a host without them.

public struct Data {
    var bytes: [UInt8]
    public init() { bytes = [] }
    public init(bytes: UnsafePointer<UInt8>, count: Int) {
        self.bytes = Array(UnsafeBufferPointer(start: bytes, count: count))
    }
    public var isEmpty: Bool { bytes.isEmpty }
    public var count: Int { bytes.count }
    public func withUnsafeBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R {
        try bytes.withUnsafeBytes(body)
    }
}

public enum DispatchQoS { case userInitiated }

public final class DispatchQueue {
    public init(label: String, qos: DispatchQoS) {}
    public func sync<T>(execute work: () throws -> T) rethrows -> T { try work() }
    public func async(execute work: @escaping () -> Void) { work() }
}

public func NSLog(_ s: String) {}

public extension String {
    enum Encoding { case utf8 }
    init?<S: Sequence>(bytes: S, encoding: Encoding) where S.Element == UInt8 {
        self = String(decoding: Array(bytes), as: UTF8.self)
    }
}
