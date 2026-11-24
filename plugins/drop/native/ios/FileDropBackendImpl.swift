import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
import UniformTypeIdentifiers
#endif

/// Backend of `istmo.drop` on iPadOS (and iPhone, for drags inside the
/// app): files dragged from another app onto the app's window.
///
/// Nothing listens until Rust calls `listen`. Then a `UIDropInteraction`
/// sits on the key window's root view, and moves to a new one when a
/// scene becomes active. A drag carrying none of the accepted types is
/// refused while it hovers: the system shows it cannot be dropped here.
///
/// On drop each file is copied, inside its load callback (the system
/// deletes its copy right after), into
/// `<caches>/istmo-drop/incoming/<uuid>/`, and one `DroppedFiles` goes on
/// the `istmo.drop.files` queue. Files above the size limit and
/// unreadable ones are listed as skipped. The copies are the app's: Rust
/// deletes them with `DropInbox::cleanup`; leftovers older than a day go
/// when the backend starts.
public final class FileDropBackendImpl: NSObject, FileDropBackend {
    // Same values as `DROP_CHANNEL`, `DROP_QUEUE_CAPACITY`, `DROP_DIR` and
    // `DEFAULT_MAX_BYTES` in the Rust crate.
    private static let channel = "istmo.drop.files"
    private static let capacity: UInt32 = 8
    private static let incomingDir = "istmo-drop/incoming"
    private static let defaultMaxBytes: UInt64 = 512 * 1024 * 1024
    private static let maxAge: TimeInterval = 24 * 60 * 60

    private let codecs = FileDropCodecsImpl()
    /// Main thread only.
    private var options: DropOptions?
    #if canImport(UIKit)
    private var interaction: UIDropInteraction?
    private var sceneObserver: NSObjectProtocol?
    #endif

    public override init() {
        super.init()
        DispatchQueue.global(qos: .utility).async { Self.pruneOld() }
    }

    public func listen(options: DropOptions) async throws {
        #if canImport(UIKit)
        await MainActor.run {
            self.options = options
            self.attach()
            self.followScenes()
        }
        #else
        throw DropError.unsupported
        #endif
    }

    public func stop() async throws {
        #if canImport(UIKit)
        await MainActor.run {
            self.options = nil
            self.detach()
            if let observer = self.sceneObserver {
                NotificationCenter.default.removeObserver(observer)
                self.sceneObserver = nil
            }
        }
        #endif
    }

    private static var incomingRoot: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent(incomingDir, isDirectory: true)
    }

    private static func pruneOld() {
        let root = incomingRoot
        let folders = (try? FileManager.default.contentsOfDirectory(
            at: root, includingPropertiesForKeys: [.contentModificationDateKey]
        )) ?? []
        let cutoff = Date().addingTimeInterval(-maxAge)
        for folder in folders {
            let modified = (try? folder.resourceValues(forKeys: [.contentModificationDateKey]))?
                .contentModificationDate ?? .distantPast
            if modified < cutoff {
                try? FileManager.default.removeItem(at: folder)
            }
        }
    }

    /// Without path separators, control characters or bidi overrides.
    private static func safeName(_ name: String?) -> String {
        let bidi: Set<Character> = [
            "\u{202A}", "\u{202B}", "\u{202C}", "\u{202D}", "\u{202E}",
            "\u{2066}", "\u{2067}", "\u{2068}", "\u{2069}", "\u{200E}", "\u{200F}",
        ]
        var cleaned = String((name ?? "").filter { character in
            !bidi.contains(character)
                && !character.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
        })
        cleaned = cleaned.replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: ":", with: "_")
            .trimmingCharacters(in: .whitespaces)
        while cleaned.hasPrefix(".") { cleaned.removeFirst() }
        cleaned = String(cleaned.prefix(120))
        return cleaned.isEmpty ? "file" : cleaned
    }
}

#if canImport(UIKit)
extension FileDropBackendImpl: UIDropInteractionDelegate {
    @MainActor
    private func rootView() -> UIView? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? scenes.first?.windows.first
        return window?.rootViewController?.view ?? window
    }

    @MainActor
    private func attach() {
        guard let view = rootView() else { return }
        if let interaction = interaction, interaction.view === view { return }
        detach()
        let next = UIDropInteraction(delegate: self)
        view.addInteraction(next)
        interaction = next
    }

    @MainActor
    private func detach() {
        if let interaction = interaction {
            interaction.view?.removeInteraction(interaction)
        }
        interaction = nil
    }

    /// The window may come later than `listen` (cold start) or change
    /// (another scene): attach again whenever a scene becomes active.
    @MainActor
    private func followScenes() {
        guard sceneObserver == nil else { return }
        sceneObserver = NotificationCenter.default.addObserver(
            forName: UIScene.didActivateNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self = self, self.options != nil else { return }
                self.attach()
            }
        }
    }

    /// Types the window takes: the MIME types asked for, or any file.
    private func acceptedTypes() -> [UTType] {
        let mimeTypes = options?.mimeTypes ?? []
        if mimeTypes.isEmpty { return [.item] }
        return mimeTypes.compactMap { mime in
            switch mime {
            case "*/*": return .item
            case "image/*": return .image
            case "text/*": return .text
            case "audio/*": return .audio
            case "video/*": return .movie
            default: return UTType(mimeType: mime)
            }
        }
    }

    public func dropInteraction(_ interaction: UIDropInteraction, canHandle session: UIDropSession) -> Bool {
        guard options != nil else { return false }
        return session.hasItemsConforming(toTypeIdentifiers: acceptedTypes().map(\.identifier))
    }

    public func dropInteraction(_ interaction: UIDropInteraction, sessionDidUpdate session: UIDropSession) -> UIDropProposal {
        UIDropProposal(operation: options == nil ? .forbidden : .copy)
    }

    public func dropInteraction(_ interaction: UIDropInteraction, performDrop session: UIDropSession) {
        guard let options = options else { return }
        let accepted = acceptedTypes()
        let limit = options.maxBytes == 0 ? Self.defaultMaxBytes : options.maxBytes
        let location = interaction.view.map { session.location(in: $0) }
        let folder = Self.incomingRoot.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let collected = Collected()
        let group = DispatchGroup()
        for (index, item) in session.items.enumerated() {
            let provider = item.itemProvider
            let name = Self.safeName(provider.suggestedName)
            // The most specific type the provider has that the window takes.
            guard let type = provider.registeredTypeIdentifiers
                .compactMap(UTType.init)
                .first(where: { candidate in accepted.contains { candidate.conforms(to: $0) } })
            else {
                collected.skip(SkippedFile(displayName: name, reason: .notAFile))
                continue
            }
            group.enter()
            provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, _ in
                defer { group.leave() }
                // The file at `url` is deleted when this returns: copy now.
                guard let url = url else {
                    collected.skip(SkippedFile(displayName: name, reason: .unreadable))
                    return
                }
                let size = (try? url.resourceValues(forKeys: [.fileSizeKey]))?.fileSize.map(UInt64.init) ?? 0
                if size > limit {
                    collected.skip(SkippedFile(displayName: name, reason: .tooLarge))
                    return
                }
                do {
                    try FileManager.default.createDirectory(at: folder, withIntermediateDirectories: true)
                    let target = Self.uniqueFile(in: folder, name: name)
                    try FileManager.default.copyItem(at: url, to: target)
                    collected.add(index, DroppedFile(
                        path: target.path,
                        displayName: name,
                        mimeType: type.preferredMIMEType,
                        size: size
                    ))
                } catch {
                    collected.skip(SkippedFile(displayName: name, reason: .unreadable))
                }
            }
        }
        group.notify(queue: .global(qos: .userInitiated)) { [codecs] in
            let (files, skipped) = collected.take()
            if files.isEmpty && skipped.isEmpty { return }
            let dropped = DroppedFiles(
                files: files,
                skipped: skipped,
                position: location.map { DropPoint(x: Float($0.x), y: Float($0.y)) },
                receivedAtMs: UInt64(Date().timeIntervalSince1970 * 1000)
            )
            var out = Data()
            codecs.writeDroppedFiles(&out, dropped)
            IstmoRuntime.shared.publishEarlyQueue(
                channel: Self.channel, capacity: Self.capacity, payload: out
            )
        }
    }

    private static func uniqueFile(in folder: URL, name: String) -> URL {
        var candidate = folder.appendingPathComponent(name)
        var n = 1
        let stem = (name as NSString).deletingPathExtension
        let ext = (name as NSString).pathExtension
        while FileManager.default.fileExists(atPath: candidate.path) {
            let next = ext.isEmpty ? "\(stem) (\(n))" : "\(stem) (\(n)).\(ext)"
            candidate = folder.appendingPathComponent(next)
            n += 1
        }
        return candidate
    }
}

/// What the load callbacks of one drop gather, from any thread.
private final class Collected: @unchecked Sendable {
    private let lock = NSLock()
    private var files: [(Int, DroppedFile)] = []
    private var skipped: [SkippedFile] = []

    /// `index` is the file's place in the drag; loads finish in any order.
    func add(_ index: Int, _ file: DroppedFile) {
        lock.lock()
        files.append((index, file))
        lock.unlock()
    }

    func skip(_ file: SkippedFile) {
        lock.lock()
        skipped.append(file)
        lock.unlock()
    }

    func take() -> ([DroppedFile], [SkippedFile]) {
        lock.lock()
        defer { lock.unlock() }
        return (files.sorted { $0.0 < $1.0 }.map(\.1), skipped)
    }
}
#endif
