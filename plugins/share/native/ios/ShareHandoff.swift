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
/// <group>/istmo-share/inbox/<uuid>/share.json
/// <group>/istmo-share/inbox/<uuid>/<file>…
/// ```
///
/// Entries are written to `<uuid>.partial/` and renamed when complete,
/// so readers never see half-written shares. The macOS drain in
/// `istmo_share::desktop::drain_app_group_inbox` reads the same format.
public enum IstmoShareHandoff {

    /// `Info.plist` key (app and extension) holding the App Group id.
    public static let appGroupInfoKey = "IstmoShareAppGroup"

    /// `Info.plist` key holding the URL scheme the extension opens to
    /// wake the app (iOS). Optional.
    public static let urlSchemeInfoKey = "IstmoShareURLScheme"

    /// Host part of the wake-up URL: `<scheme>://istmo-share`.
    public static let wakeHost = "istmo-share"

    public struct Manifest: Codable {
        public var text: String?
        public var url: String?
        public var subject: String?
        public var files: [File]
        public var sourceApp: String?
        public var receivedAtMs: UInt64?

        public init(text: String? = nil, url: String? = nil, subject: String? = nil, files: [File] = [],
                    sourceApp: String? = nil, receivedAtMs: UInt64? = nil) {
            self.text = text
            self.url = url
            self.subject = subject
            self.files = files
            self.sourceApp = sourceApp
            self.receivedAtMs = receivedAtMs
        }
    }

    public struct File: Codable {
        /// File name inside the entry directory.
        public var file: String
        /// Original display name.
        public var name: String
        public var mimeType: String?

        public init(file: String, name: String, mimeType: String?) {
            self.file = file
            self.name = name
            self.mimeType = mimeType
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

    /// Extension side: write `share.json` and publish the entry.
    public static func commit(entry: URL, manifest: Manifest) throws {
        let data = try JSONEncoder().encode(manifest)
        try data.write(to: entry.appendingPathComponent("share.json"), options: .atomic)
        let final = entry.deletingPathExtension()
        try FileManager.default.moveItem(at: entry, to: final)
    }

    /// Unique, sanitised file name for `name` inside `entry`.
    public static func fileName(for name: String, in entry: URL) -> String {
        let base = sanitize(name)
        var candidate = base
        var counter = 1
        while FileManager.default.fileExists(atPath: entry.appendingPathComponent(candidate).path)
            || candidate == "share.json" {
            let ext = (base as NSString).pathExtension
            let stem = (base as NSString).deletingPathExtension
            candidate = ext.isEmpty ? "\(stem)-\(counter)" : "\(stem)-\(counter).\(ext)"
            counter += 1
        }
        return candidate
    }

    static func sanitize(_ name: String) -> String {
        let base = (name as NSString).lastPathComponent
        let forbidden = CharacterSet(charactersIn: "/\\:*?\"<>|").union(.controlCharacters)
        let cleaned = String(base.unicodeScalars.map { forbidden.contains($0) ? "_" : Character($0) })
        return cleaned.trimmingCharacters(in: CharacterSet(charactersIn: ".")).isEmpty ? "shared" : cleaned
    }

    public static func nowMs() -> UInt64 {
        UInt64(Date().timeIntervalSince1970 * 1000)
    }
}
