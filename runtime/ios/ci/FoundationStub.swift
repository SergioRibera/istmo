// Minimal stand-ins for the Foundation / Dispatch API surface the runtime
// and the code `istmo-build` generates use, so the sources can be
// type-checked on a host without them. `Data` keeps slice indices like
// the real type (a slice's startIndex is its offset in the parent).
public struct Data: Equatable, Sequence, CustomStringConvertible {
    public typealias Index = Int
    var bytes: [UInt8]
    public private(set) var startIndex: Int

    public init() { bytes = []; startIndex = 0 }
    public init(_ b: [UInt8]) { bytes = b; startIndex = 0 }
    init(bytes b: [UInt8], start: Int) { bytes = b; startIndex = start }
    public init(bytes: UnsafePointer<UInt8>, count: Int) {
        self.bytes = Array(UnsafeBufferPointer(start: bytes, count: count))
        startIndex = 0
    }
    public var endIndex: Int { startIndex + bytes.count }
    public var count: Int { bytes.count }
    public var isEmpty: Bool { bytes.isEmpty }
    public subscript(i: Int) -> UInt8 {
        precondition(i >= startIndex && i < endIndex, "Data index \(i) out of \(startIndex)..<\(endIndex)")
        return bytes[i - startIndex]
    }
    public subscript(r: Range<Int>) -> Data {
        Data(bytes: Array(bytes[(r.lowerBound - startIndex) ..< (r.upperBound - startIndex)]), start: r.lowerBound)
    }
    public subscript(r: PartialRangeFrom<Int>) -> Data { self[r.lowerBound ..< endIndex] }
    public func subdata(in r: Range<Int>) -> Data {
        precondition(r.lowerBound >= startIndex && r.upperBound <= endIndex, "subdata out of range")
        return Data(Array(bytes[(r.lowerBound - startIndex) ..< (r.upperBound - startIndex)]))
    }
    public mutating func append(_ b: UInt8) { bytes.append(b) }
    public mutating func append(contentsOf b: [UInt8]) { bytes.append(contentsOf: b) }
    public mutating func append(_ d: Data) { bytes.append(contentsOf: d.bytes) }
    public func withUnsafeBytes<R>(_ body: (UnsafeRawBufferPointer) throws -> R) rethrows -> R {
        try bytes.withUnsafeBytes(body)
    }
    public func makeIterator() -> IndexingIterator<[UInt8]> { bytes.makeIterator() }
    public static func == (a: Data, b: Data) -> Bool { a.bytes == b.bytes }
    public var description: String { "\(count) bytes" }
}

public func NSLog(_ s: String) {}

public extension String {
    enum Encoding { case utf8 }
    /// Strict like Foundation: malformed UTF-8 yields nil.
    init?(data: Data, encoding: Encoding) {
        var it = data.bytes.makeIterator()
        var dec = UTF8()
        var out = String.UnicodeScalarView()
        loop: while true {
            switch dec.decode(&it) {
            case .scalarValue(let s): out.append(s)
            case .emptyInput: break loop
            case .error: return nil
            }
        }
        self = String(out)
    }
}

public enum DispatchQoS { case userInitiated }

public final class DispatchQueue {
    public init(label: String, qos: DispatchQoS) {}
    public func sync<T>(execute work: () throws -> T) rethrows -> T { try work() }
    public func async(execute work: @escaping () -> Void) { work() }
}

public extension String {
    init?<S: Sequence>(bytes: S, encoding: Encoding) where S.Element == UInt8 {
        self = String(decoding: Array(bytes), as: UTF8.self)
    }
}

public final class NSLock {
    public init() {}
    public func lock() {}
    public func unlock() {}
    public func withLock<R>(_ body: () throws -> R) rethrows -> R { try body() }
}

public typealias TimeInterval = Double
public struct Date {
    public init(timeIntervalSinceNow: TimeInterval) {}
}
