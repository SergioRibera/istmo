import Foundation
import IstmoRuntime

/// App side of the Share Extension hand-off (see ``IstmoShareHandoff``).
///
/// Moves every complete inbox entry into `Caches/istmo-share/incoming/`,
/// publishes it as an `IncomingShare` on the `istmo.share.incoming`
/// early-event queue (buffered until Rust subscribes), and deletes the
/// entry. Call it on launch, when the app becomes active, and when the
/// extension's wake-up URL arrives:
///
/// ```swift
/// // at launch
/// IstmoShareInbox.observeHandoffs()
///
/// func sceneDidBecomeActive(_ scene: UIScene) { IstmoShareInbox.drain() }
///
/// func scene(_ scene: UIScene, openURLContexts contexts: Set<UIOpenURLContext>) {
///     if contexts.contains(where: { IstmoShareInbox.isWakeURL($0.url) }) { IstmoShareInbox.drain() }
/// }
/// ```
public enum IstmoShareInbox {

    /// Mirrors `istmo_share::INCOMING_CHANNEL`.
    public static let incomingChannel = "istmo.share.incoming"

    /// Mirrors `istmo_share::INCOMING_QUEUE_CAPACITY`.
    public static let incomingQueueCapacity: UInt32 = 8

    /// Drain as soon as the Share Extension commits an entry, while the
    /// app process is alive (the extension posts a Darwin notification).
    /// Call once at launch; the wake-up URL and activation drains remain
    /// the fallback for a suspended or terminated app.
    public static func observeHandoffs(appGroup: String? = nil) {
        guard let group = appGroup ?? IstmoShareHandoff.configuredAppGroup() else { return }
        let name = IstmoShareHandoff.handoffNotificationName(appGroup: group) as CFString
        CFNotificationCenterAddObserver(
            CFNotificationCenterGetDarwinNotifyCenter(),
            nil,
            { _, _, _, _, _ in
                DispatchQueue.main.async { IstmoShareInbox.drain() }
            },
            name,
            nil,
            .deliverImmediately
        )
    }

    /// `true` for the `<scheme>://istmo-share` URL the extension opens.
    public static func isWakeURL(_ url: URL) -> Bool {
        url.host == IstmoShareHandoff.wakeHost
    }

    /// Mirrors `istmo_share::INCOMING_RETENTION`: received copies the app
    /// never cleaned up are deleted after this long.
    public static let incomingRetention: TimeInterval = 7 * 24 * 60 * 60

    private static func pruneStale(_ root: URL) {
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.contentModificationDateKey]) else {
            return
        }
        let cutoff = Date().addingTimeInterval(-incomingRetention)
        for entry in entries {
            let modified = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            if let modified = modified, modified < cutoff {
                try? fm.removeItem(at: entry)
            }
        }
    }

    /// Publish every pending share. Returns how many were published.
    @discardableResult
    public static func drain(appGroup: String? = nil) -> Int {
        guard let group = appGroup ?? IstmoShareHandoff.configuredAppGroup(),
              let inbox = IstmoShareHandoff.inboxURL(appGroup: group) else {
            return 0
        }
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(at: inbox, includingPropertiesForKeys: nil) else {
            return 0
        }
        let caches = fm.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("istmo-share/incoming", isDirectory: true)
        pruneStale(caches)
        var published = 0
        for entry in entries where entry.pathExtension != "partial" {
            defer { try? fm.removeItem(at: entry) }
            guard let data = try? Data(contentsOf: entry.appendingPathComponent("share.json")),
                  let manifest = try? JSONDecoder().decode(IstmoShareHandoff.Manifest.self, from: data) else {
                continue
            }
            let dest = caches.appendingPathComponent(entry.lastPathComponent, isDirectory: true)
            try? fm.createDirectory(at: dest, withIntermediateDirectories: true)
            let files: [IncomingFile] = manifest.files.compactMap { file in
                let src = entry.appendingPathComponent(file.file)
                let target = dest.appendingPathComponent(file.file)
                guard (try? fm.moveItem(at: src, to: target)) != nil else { return nil }
                let size = (try? fm.attributesOfItem(atPath: target.path)[.size] as? NSNumber)?.uint64Value
                return IncomingFile(path: target.path, name: file.name, mimeType: file.mimeType, size: size)
            }
            let share = IncomingShare(
                text: manifest.text,
                url: manifest.url,
                subject: manifest.subject,
                files: files,
                sourceApp: manifest.sourceApp,
                receivedAtMs: manifest.receivedAtMs ?? IstmoShareHandoff.nowMs(),
                targetId: manifest.targetId
            )
            var payload = Data()
            ShareCodecsImpl().writeIncomingShare(&payload, share)
            IstmoRuntime.shared.publishEarlyQueue(
                channel: incomingChannel,
                capacity: incomingQueueCapacity,
                payload: payload
            )
            published += 1
        }
        return published
    }
}
