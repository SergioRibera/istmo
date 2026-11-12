import Foundation

/// Hand-off format between the Share Extension and the containing app.
///
/// Foundation-only on purpose: the extension target compiles this file
/// too (add `native/ios/ShareHandoff.swift` to its sources), while the
/// app-only drain lives in `ShareInboxDrain.swift`.
///
/// Layout inside the App Group container:
///
/// ```text
/// <group>/istmo-share/inbox/<uuid>/share.bin
/// <group>/istmo-share/inbox/<uuid>/<file>…
/// ```
///
/// `share.bin` is an `IncomingShare` in the istmo wire layout (bincode 2
/// `standard()`), byte for byte what `ShareCodecsImpl.writeIncomingShare`
/// produces; each file's `path` is its name inside the entry directory.
/// The extension cannot link `IstmoRuntime`, so ``Manifest`` carries its
/// own copy of that codec. Entries are written to `<uuid>.partial/` and
/// renamed when complete, so readers never see half-written shares. The
/// macOS drain in `istmo_share::desktop::drain_app_group_inbox` reads the
/// same format; its `SWIFT_MANIFEST` test pins the bytes.
public enum IstmoShareHandoff {

    /// `Info.plist` key (app and extension) holding the App Group id.
    public static let appGroupInfoKey = "IstmoShareAppGroup"

    /// `Info.plist` key holding the URL scheme the extension opens to
    /// wake the app (iOS). Optional.
    public static let urlSchemeInfoKey = "IstmoShareURLScheme"

    /// Host part of the wake-up URL: `<scheme>://istmo-share`.
    public static let wakeHost = "istmo-share"

    /// Manifest file name inside an entry.
    public static let manifestName = "share.bin"

    /// Darwin notification posted after every committed entry, so a
    /// running app drains immediately. Scoped by App Group so unrelated
    /// apps using istmo do not wake each other.
    public static func handoffNotificationName(appGroup: String) -> String {
        "\(appGroup).istmo-share.handoff"
    }

    /// Extension side: tell a running containing app that a share is
    /// waiting. Works whether or not the app is in the foreground, as
    /// long as its process is alive.
    public static func postHandoff(appGroup: String) {
        let name = handoffNotificationName(appGroup: appGroup) as CFString
        CFNotificationCenterPostNotification(
            CFNotificationCenterGetDarwinNotifyCenter(),
            CFNotificationName(name),
            nil,
            nil,
            true
        )
    }

    /// Mirror of `IncomingShare`, field for field and in wire order.
    public struct Manifest {
        public var text: String?
        public var url: String?
        public var subject: String?
        public var files: [File]
        public var sourceApp: String?
        public var receivedAtMs: UInt64?
        /// `conversationIdentifier` of the donated share target the user
        /// picked, if any.
        public var targetId: String?

        public init(text: String? = nil, url: String? = nil, subject: String? = nil, files: [File] = [],
                    sourceApp: String? = nil, receivedAtMs: UInt64? = nil, targetId: String? = nil) {
            self.text = text
            self.url = url
            self.subject = subject
            self.files = files
            self.sourceApp = sourceApp
            self.receivedAtMs = receivedAtMs
            self.targetId = targetId
        }

        /// `IncomingShare` wire bytes.
        public func encode() -> Data {
            var out = Data()
            Wire.writeOption(&out, text, Wire.writeString)
            Wire.writeOption(&out, url, Wire.writeString)
            Wire.writeOption(&out, subject, Wire.writeString)
            Wire.writeVarint(&out, UInt64(files.count))
            for file in files {
                file.encode(into: &out)
            }
            Wire.writeOption(&out, sourceApp, Wire.writeString)
            Wire.writeOption(&out, receivedAtMs, Wire.writeVarint)
            Wire.writeOption(&out, targetId, Wire.writeString)
            return out
        }

        /// Parse `IncomingShare` wire bytes, refusing trailing data.
        public static func decode(_ data: Data) throws -> Manifest {
            var reader = Wire.Reader(Array(data))
            let text = try reader.option { try $0.string() }
            let url = try reader.option { try $0.string() }
            let subject = try reader.option { try $0.string() }
            let files = try reader.vec { try File.decode(&$0) }
            let sourceApp = try reader.option { try $0.string() }
            let receivedAtMs = try reader.option { try $0.varint() }
            let targetId = try reader.option { try $0.string() }
            guard reader.atEnd else { throw Wire.Error.malformed }
            return Manifest(text: text, url: url, subject: subject, files: files,
                            sourceApp: sourceApp, receivedAtMs: receivedAtMs, targetId: targetId)
        }
    }

    /// Mirror of `IncomingFile`.
    public struct File {
        /// File name inside the entry directory (`IncomingFile.path`).
        public var file: String
        /// Original display name.
        public var name: String
        public var mimeType: String?
        public var size: UInt64?

        public init(file: String, name: String, mimeType: String?, size: UInt64? = nil) {
            self.file = file
            self.name = name
            self.mimeType = mimeType
            self.size = size
        }

        func encode(into out: inout Data) {
            Wire.writeString(&out, file)
            Wire.writeString(&out, name)
            Wire.writeOption(&out, mimeType, Wire.writeString)
            Wire.writeOption(&out, size, Wire.writeVarint)
        }

        static func decode(_ reader: inout Wire.Reader) throws -> File {
            let file = try reader.string()
            let name = try reader.string()
            let mimeType = try reader.option { try $0.string() }
            let size = try reader.option { try $0.varint() }
            return File(file: file, name: name, mimeType: mimeType, size: size)
        }
    }

    /// The subset of the bincode 2 `standard()` layout a manifest needs:
    /// varint integers (`< 251` inline, else a 251 / 252 / 253 marker and
    /// a little-endian u16 / u32 / u64), length-prefixed UTF-8 strings,
    /// `0` / `1` option tags and length-prefixed sequences.
    enum Wire {
        enum Error: Swift.Error {
            case malformed
        }

        static func writeVarint(_ out: inout Data, _ value: UInt64) {
            if value < 251 {
                out.append(UInt8(value))
            } else if value <= UInt64(UInt16.max) {
                out.append(251)
                appendLE(&out, value, bytes: 2)
            } else if value <= UInt64(UInt32.max) {
                out.append(252)
                appendLE(&out, value, bytes: 4)
            } else {
                out.append(253)
                appendLE(&out, value, bytes: 8)
            }
        }

        static func writeString(_ out: inout Data, _ value: String) {
            let bytes = Array(value.utf8)
            writeVarint(&out, UInt64(bytes.count))
            out.append(contentsOf: bytes)
        }

        static func writeOption<T>(_ out: inout Data, _ value: T?, _ write: (inout Data, T) -> Void) {
            if let value {
                out.append(1)
                write(&out, value)
            } else {
                out.append(0)
            }
        }

        private static func appendLE(_ out: inout Data, _ value: UInt64, bytes: Int) {
            for shift in 0..<bytes {
                out.append(UInt8(truncatingIfNeeded: value >> (8 * UInt64(shift))))
            }
        }

        struct Reader {
            private let bytes: [UInt8]
            private var offset = 0

            init(_ bytes: [UInt8]) {
                self.bytes = bytes
            }

            var atEnd: Bool { offset == bytes.count }

            mutating func byte() throws -> UInt8 {
                guard offset < bytes.count else { throw Error.malformed }
                defer { offset += 1 }
                return bytes[offset]
            }

            mutating func varint() throws -> UInt64 {
                let tag = try byte()
                let width: Int
                switch tag {
                case 0..<251: return UInt64(tag)
                case 251: width = 2
                case 252: width = 4
                case 253: width = 8
                default: throw Error.malformed
                }
                var value: UInt64 = 0
                for shift in 0..<width {
                    value |= UInt64(try byte()) << (8 * UInt64(shift))
                }
                return value
            }

            mutating func string() throws -> String {
                let count = try varint()
                guard count <= UInt64(bytes.count - offset) else { throw Error.malformed }
                let end = offset + Int(count)
                guard let value = String(bytes: bytes[offset..<end], encoding: .utf8) else {
                    throw Error.malformed
                }
                offset = end
                return value
            }

            mutating func option<T>(_ read: (inout Reader) throws -> T) throws -> T? {
                switch try byte() {
                case 0: return nil
                case 1: return try read(&self)
                default: throw Error.malformed
                }
            }

            mutating func vec<T>(_ read: (inout Reader) throws -> T) throws -> [T] {
                let count = try varint()
                // Every element takes at least one byte; a larger count
                // is corrupt and must not drive a huge allocation.
                guard count <= UInt64(bytes.count - offset) else { throw Error.malformed }
                var items: [T] = []
                items.reserveCapacity(Int(count))
                for _ in 0..<count {
                    items.append(try read(&self))
                }
                return items
            }
        }
    }

    /// App Group id from the bundle's `Info.plist`.
    public static func configuredAppGroup(bundle: Bundle = .main) -> String? {
        (bundle.object(forInfoDictionaryKey: appGroupInfoKey) as? String).flatMap { $0.isEmpty ? nil : $0 }
    }

    /// `<group>/istmo-share/inbox`, or `nil` when the group is not in
    /// the target's entitlements.
    public static func inboxURL(appGroup: String) -> URL? {
        FileManager.default
            .containerURL(forSecurityApplicationGroupIdentifier: appGroup)?
            .appendingPathComponent("istmo-share/inbox", isDirectory: true)
    }

    /// Extension side: start a new entry. Copy files into the returned
    /// directory, then call ``commit(entry:manifest:)``.
    public static func beginEntry(appGroup: String) throws -> URL {
        guard let inbox = inboxURL(appGroup: appGroup) else {
            throw CocoaError(.fileNoSuchFile, userInfo: [NSLocalizedDescriptionKey: "App Group \(appGroup) unavailable"])
        }
        let entry = inbox.appendingPathComponent("\(UUID().uuidString).partial", isDirectory: true)
        try FileManager.default.createDirectory(at: entry, withIntermediateDirectories: true)
        return entry
    }

    /// Extension side: write `share.bin` and publish the entry.
    public static func commit(entry: URL, manifest: Manifest) throws {
        try manifest.encode().write(to: entry.appendingPathComponent(manifestName), options: .atomic)
        let final = entry.deletingPathExtension()
        try FileManager.default.moveItem(at: entry, to: final)
    }

    /// Unique, sanitised file name for `name` inside `entry`.
    public static func fileName(for name: String, in entry: URL) -> String {
        let base = sanitize(name)
        var candidate = base
        var counter = 1
        while FileManager.default.fileExists(atPath: entry.appendingPathComponent(candidate).path)
            || candidate == manifestName {
            let ext = (base as NSString).pathExtension
            let stem = (base as NSString).deletingPathExtension
            candidate = ext.isEmpty ? "\(stem)-\(counter)" : "\(stem)-\(counter).\(ext)"
            counter += 1
        }
        return candidate
    }

    /// File name that cannot escape the directory it is joined to.
    ///
    /// Port of `sanitize_file_name` in `plugins/share/src/lib.rs`; keep
    /// the rules identical and check them against its `sanitize_vectors`
    /// test:
    /// 1. split on `/` and `\` and keep the last non-empty component;
    /// 2. replace `< > : " | ? *` and C0 / C1 controls (U+0000–U+001F,
    ///    U+007F–U+009F) with `_`;
    /// 3. a result made only of dots (or empty) becomes `shared`.
    public static func sanitize(_ name: String) -> String {
        // Unicode scalars, not `Character`s: a separator followed by a
        // combining mark is one grapheme but still a separator.
        let parts = Array(name.unicodeScalars).split(whereSeparator: { $0 == "/" || $0 == "\\" })
        var cleaned = String.UnicodeScalarView()
        for scalar in parts.last ?? [] {
            let forbidden = "<>:\"|?*".unicodeScalars.contains(scalar)
                || scalar.value <= 0x1F
                || (0x7F...0x9F).contains(scalar.value)
            cleaned.append(forbidden ? "_" : scalar)
        }
        let result = String(cleaned)
        return result.unicodeScalars.allSatisfy({ $0 == "." }) ? "shared" : result
    }

    public static func nowMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }
}
