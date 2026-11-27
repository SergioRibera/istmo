// Minimal stand-ins for the Foundation / Dispatch API surface the runtime
// and the code `istmo-build` generates use, so the sources can be
// type-checked on a host without them. `Data` keeps slice indices like
// the real type (a slice's startIndex is its offset in the parent).
public struct Data: Hashable, Sequence, CustomStringConvertible, Sendable {
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
    public func hash(into hasher: inout Hasher) { hasher.combine(bytes) }
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

public enum DispatchQoS {
    case userInitiated
    public enum QoSClass { case `default`, userInitiated }
}

public final class DispatchQueue {
    public init(label: String, qos: DispatchQoS) {}
    public static func global(qos: DispatchQoS.QoSClass = .default) -> DispatchQueue {
        DispatchQueue(label: "global", qos: .userInitiated)
    }
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
    @available(*, noasync, message: "Use async-safe scoped locking instead")
    public func lock() {}
    @available(*, noasync, message: "Use async-safe scoped locking instead")
    public func unlock() {}
    public func withLock<R>(_ body: () throws -> R) rethrows -> R { try body() }
}

public typealias TimeInterval = Double
public struct Date {
    public init() {}
    public init(timeIntervalSinceNow: TimeInterval) {}
    public var timeIntervalSince1970: TimeInterval { 0 }
}

// Objective-C / Core Foundation object model, for the Apple framework
// stubs (CoreMedia, ScreenCaptureKit, ...) that plugin natives use.
// Core Foundation types are classes on Apple platforms; `AnyObject`
// keeps the toll-free-bridged casts plugin code writes
// (`as? [[Key: Any]]`, `as? NSDictionary`) type-checking here.
public typealias CFTypeRef = AnyObject
public typealias CFArray = AnyObject
public typealias CFDictionary = AnyObject
public typealias CFString = AnyObject
public typealias CFAllocator = AnyObject

public protocol NSObjectProtocol: AnyObject {}

open class NSObject: NSObjectProtocol {
    public init() {}
}

open class NSValue: NSObject {}

open class NSNumber: NSValue {
    public var intValue: Int { 0 }
    public var uint32Value: UInt32 { 0 }
    public var doubleValue: Double { 0 }
}

open class NSDictionary: NSObject {}
open class NSMutableDictionary: NSDictionary {
    open subscript(key: Any) -> Any? {
        get { nil }
        set {}
    }
}

open class NSError: NSObject, @unchecked Sendable, Error {
    public var domain: String { "" }
    public var code: Int { 0 }
    public var localizedDescription: String { "" }
}

public struct Notification {
    public struct Name: Hashable, Sendable {
        public let rawValue: String
        public init(_ rawValue: String) { self.rawValue = rawValue }
    }
    public let name: Name
}

public final class OperationQueue {
    public static let main = OperationQueue()
}

public final class NotificationCenter {
    public static let `default` = NotificationCenter()
    public func addObserver(forName name: Notification.Name?, object obj: Any?, queue: OperationQueue?,
                            using block: @escaping @Sendable (Notification) -> Void) -> NSObjectProtocol { NSObject() }
    public func removeObserver(_ observer: Any) {}
}

public extension DispatchQueue {
    static var main: DispatchQueue { DispatchQueue(label: "main", qos: .userInitiated) }
}

public final class Bundle {
    public static let main = Bundle()
    public func object(forInfoDictionaryKey key: String) -> Any? { nil }
}

/// libc's `free` (Foundation re-exports Darwin).
public func free(_ ptr: UnsafeMutableRawPointer?) {}

// Files, dates and timers, for the notifications native backend.
public extension Error {
    var localizedDescription: String { "" }
}

public struct URL: Hashable, Sendable {
    public let path: String
    public init(fileURLWithPath path: String) { self.path = path }
    public init?(string: String) { path = string }
    public func appendingPathComponent(_ component: String) -> URL { URL(fileURLWithPath: path + "/" + component) }
    public func appendingPathComponent(_ component: String, isDirectory: Bool) -> URL {
        appendingPathComponent(component)
    }
    public func deletingLastPathComponent() -> URL { self }
    public var pathExtension: String { "" }
}

public extension Data {
    struct WritingOptions: OptionSet, Sendable {
        public let rawValue: UInt
        public init(rawValue: UInt) { self.rawValue = rawValue }
        public static let atomic = WritingOptions(rawValue: 1)
    }
    init(contentsOf url: URL) throws { self.init() }
    func write(to url: URL, options: WritingOptions = []) throws {}
}

public final class FileManager: @unchecked Sendable {
    public enum SearchPathDirectory { case applicationSupportDirectory, cachesDirectory }
    public struct SearchPathDomainMask: OptionSet, Sendable {
        public let rawValue: UInt
        public init(rawValue: UInt) { self.rawValue = rawValue }
        public static let userDomainMask = SearchPathDomainMask(rawValue: 1)
    }
    public static let `default` = FileManager()
    public var temporaryDirectory: URL { URL(fileURLWithPath: "/tmp") }
    public func urls(for directory: SearchPathDirectory, in domainMask: SearchPathDomainMask) -> [URL] { [] }
    public func createDirectory(at url: URL, withIntermediateDirectories: Bool,
                                attributes: [String: Any]?) throws {}
    public func copyItem(at srcURL: URL, to dstURL: URL) throws {}
    public func removeItem(at url: URL) throws {}
}

public struct UUID: Hashable, Sendable {
    public init() {}
    public var uuidString: String { "" }
}

public extension Date {
    init(timeIntervalSince1970: TimeInterval) { self.init() }
}

public struct TimeZone: Hashable, Sendable {
    public init?(identifier: String) {}
    public static var current: TimeZone { TimeZone(identifier: "UTC")! }
    public func secondsFromGMT(for date: Date) -> Int { 0 }
}

public struct DateComponents: Hashable, Sendable {
    public var timeZone: TimeZone?
    public var year: Int?
    public var month: Int?
    public var day: Int?
    public var hour: Int?
    public var minute: Int?
    public var second: Int?
    public var weekday: Int?
    public init() {}
}

public struct Calendar: Hashable, Sendable {
    public enum Identifier: Sendable { case gregorian }
    public enum Component: Hashable, Sendable { case year, month, day, hour, minute, second, weekday }
    public var timeZone: TimeZone
    public init(identifier: Identifier) { timeZone = TimeZone(identifier: "UTC")! }
    public func dateComponents(_ components: Set<Component>, from date: Date) -> DateComponents { DateComponents() }
}

public struct DispatchTime: Sendable {
    public static func now() -> DispatchTime { DispatchTime() }
    public static func + (time: DispatchTime, seconds: Double) -> DispatchTime { time }
}

public extension DispatchQueue {
    func asyncAfter(deadline: DispatchTime, execute work: @escaping @Sendable () -> Void) {}
}

public extension Bundle {
    func url(forResource name: String?, withExtension ext: String?) -> URL? { nil }
}

public struct PersonNameComponents: Hashable, Sendable {
    public init() {}
}
