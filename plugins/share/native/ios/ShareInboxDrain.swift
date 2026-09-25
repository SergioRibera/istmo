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

    /// `true` for the `<scheme>://istmo-share` URL the extension opens.
    public static func isWakeURL(_ url: URL) -> Bool {
        url.host == IstmoShareHandoff.wakeHost
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
                receivedAtMs: manifest.receivedAtMs ?? IstmoShareHandoff.nowMs()
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
