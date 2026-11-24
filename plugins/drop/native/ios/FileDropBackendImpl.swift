import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
import UniformTypeIdentifiers
#endif

/// Backend of `istmo.drop` on iPadOS and iOS: files dragged from another
/// app onto the app's windows (iPad, and iPhone since iOS 15).
///
/// Nothing listens until Rust calls `listen`. Then every window of every
/// scene gets a `UIDropInteraction`, sheets included (it sits on the
/// window, not on a view controller), and windows of scenes that connect
/// later get one too. A drag carrying none of the accepted types is not
/// handled. While one the window takes hovers, `DragState.over` goes on
/// the `istmo.drop.drag` channel (with its position, a few dozen times a
/// second at most); `receiving` while the files load, `idle` after.
///
/// On drop each file is copied, inside its load callback (the system
/// deletes its own copy right after), into
/// `<caches>/istmo-drop/incoming/<uuid>/<n>/`, and one `DroppedFiles`
/// goes on the `istmo.drop.files` queue. Folders, text and links (unless
/// the app takes `text/*`), files above the size limit and unreadable ones
/// come back as skipped. The copies are the app's: Rust deletes them with
/// `DropInbox::cleanup`; leftovers older than a day go when the backend
/// starts.
public final class FileDropBackendImpl: NSObject, FileDropBackend {
    // Same values as `DROP_CHANNEL`, `DROP_QUEUE_CAPACITY`, `DRAG_CHANNEL`,
    // `DROP_DIR` and `DEFAULT_MAX_BYTES` in the Rust crate.
    private static let channel = "istmo.drop.files"
    private static let capacity: UInt32 = 8
    private static let dragChannel = "istmo.drop.drag"
    private static let incomingDir = "istmo-drop/incoming"
    private static let defaultMaxBytes: UInt64 = 512 * 1024 * 1024
    private static let maxAge: TimeInterval = 24 * 60 * 60
    private static let moveInterval: TimeInterval = 1.0 / 30
    private static let maxNameBytes = 200

    private let codecs = FileDropCodecsImpl()
    /// Main thread only, like everything below.
    private var options: DropOptions?
    #if canImport(UIKit)
    private var accepted: [UTType] = []
    private var interactions: [ObjectIdentifier: UIDropInteraction] = [:]
    private var observers: [NSObjectProtocol] = []
    private var lastMove = Date.distantPast
    /// Files of a drop are loading: `sessionDidEnd` must not go idle.
    private var receiving = false
    #endif

    public override init() {
        super.init()
        DispatchQueue.global(qos: .utility).async { Self.pruneOld() }
    }

    public func listen(options: DropOptions) async throws {
        #if canImport(UIKit)
        let types = Self.types(for: options.mimeTypes)
        if types.isEmpty {
            throw DropError.backend("none of \(options.mimeTypes) names a known type")
        }
        await MainActor.run {
            self.options = options
            self.accepted = types
            self.attachEverywhere()
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
            for interaction in self.interactions.values {
                interaction.view?.removeInteraction(interaction)
            }
            self.interactions.removeAll()
            for observer in self.observers {
                NotificationCenter.default.removeObserver(observer)
            }
            self.observers.removeAll()
            self.publishState(.idle)
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

    /// Without path separators, control characters or bidi overrides,
    /// short enough in UTF-8 bytes for APFS once " (n)" is added, its
    /// extension kept; `ext` added when it has none.
    static func safeName(_ name: String?, ext: String?) -> String {
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
        if cleaned.isEmpty { cleaned = "file" }
        if let ext = ext, !ext.isEmpty, (cleaned as NSString).pathExtension.isEmpty {
            cleaned += ".\(ext)"
        }
        guard cleaned.utf8.count > maxNameBytes else { return cleaned }
        let suffix = (cleaned as NSString).pathExtension
        let tail = suffix.isEmpty || suffix.utf8.count > 16 ? "" : ".\(suffix)"
        var stem = tail.isEmpty ? cleaned : (cleaned as NSString).deletingPathExtension
        while stem.utf8.count + tail.utf8.count > maxNameBytes { stem.removeLast() }
        return stem + tail
    }
}

#if canImport(UIKit)
extension FileDropBackendImpl: UIDropInteractionDelegate {
    /// What `mimeTypes` names, wildcards through their supertype; every
    /// file when empty.
    static func types(for mimeTypes: [String]) -> [UTType] {
        if mimeTypes.isEmpty { return [.data] }
        return mimeTypes.compactMap { mime -> UTType? in
            switch mime {
            case "*/*": return .data
            case "image/*": return .image
            case "text/*": return .text
            case "audio/*": return .audio
            case "video/*": return .movie
            case "font/*": return .font
            case "model/*": return .threeDContent
            case "application/*": return .data
            default:
                // A type the system does not know has no real identifier.
                guard let type = UTType(mimeType: mime), !type.isDynamic else { return nil }
                return type
            }
        }
    }

    private func takesText() -> Bool {
        accepted.contains { $0.conforms(to: .text) }
    }

    @MainActor
    private func attachEverywhere() {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
        let live = Set(windows.map(ObjectIdentifier.init))
        for (id, interaction) in interactions where !live.contains(id) {
            interaction.view?.removeInteraction(interaction)
            interactions[id] = nil
        }
        for window in windows where interactions[ObjectIdentifier(window)] == nil {
            let interaction = UIDropInteraction(delegate: self)
            window.addInteraction(interaction)
            interactions[ObjectIdentifier(window)] = interaction
        }
    }

    /// Windows come later than `listen` (cold start) and with new scenes:
    /// attach again when one connects or becomes active.
    @MainActor
    private func followScenes() {
        guard observers.isEmpty else { return }
        for name in [UIScene.willConnectNotification, UIScene.didActivateNotification,
                     UIScene.didDisconnectNotification] {
            observers.append(NotificationCenter.default.addObserver(
                forName: name, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated {
                    guard let self = self, self.options != nil else { return }
                    self.attachEverywhere()
                }
            })
        }
    }

    @MainActor
    private func publishState(_ state: DragState) {
        var out = Data()
        codecs.writeDragState(&out, state)
        IstmoRuntime.shared.publishEarlyLatest(channel: Self.dragChannel, payload: out)
    }

    private func over(_ session: UIDropSession, in view: UIView?) -> DragState {
        let point = view.map { session.location(in: $0) }
        return .over(DragOver(
            count: UInt32(session.items.count),
            position: point.map { DropPoint(x: Float($0.x), y: Float($0.y)) }
        ))
    }

    public func dropInteraction(_ interaction: UIDropInteraction, canHandle session: UIDropSession) -> Bool {
        guard options != nil else { return false }
        return session.items.contains { fileType(of: $0.itemProvider) != nil }
    }

    public func dropInteraction(_ interaction: UIDropInteraction, sessionDidEnter session: UIDropSession) {
        publishState(over(session, in: interaction.view))
    }

    public func dropInteraction(_ interaction: UIDropInteraction, sessionDidUpdate session: UIDropSession) -> UIDropProposal {
        let now = Date()
        if now.timeIntervalSince(lastMove) >= Self.moveInterval {
            lastMove = now
            publishState(over(session, in: interaction.view))
        }
        return UIDropProposal(operation: options == nil ? .forbidden : .copy)
    }

    public func dropInteraction(_ interaction: UIDropInteraction, sessionDidExit session: UIDropSession) {
        publishState(.idle)
    }

    public func dropInteraction(_ interaction: UIDropInteraction, sessionDidEnd session: UIDropSession) {
        // A drop moved the state on to `receiving`; `performDrop` ends it.
        if !receiving { publishState(.idle) }
    }

    /// The most specific type of `provider` the window takes: a file, not
    /// text or a link unless the app asked for text, never a folder.
    private func fileType(of provider: NSItemProvider) -> UTType? {
        let types = provider.registeredTypeIdentifiers.compactMap(UTType.init)
        if types.contains(where: { $0.conforms(to: .directory) || $0.conforms(to: .package) }) {
            return nil
        }
        if !takesText(), !types.isEmpty,
           types.allSatisfy({ $0.conforms(to: .text) || $0.conforms(to: .url) }) {
            return nil
        }
        return types.first { candidate in
            candidate.conforms(to: .data) && accepted.contains { candidate.conforms(to: $0) }
        }
    }

    public func dropInteraction(_ interaction: UIDropInteraction, performDrop session: UIDropSession) {
        guard let options = options else { return }
        let limit = options.maxBytes == 0 ? Self.defaultMaxBytes : options.maxBytes
        let location = interaction.view.map { session.location(in: $0) }
        let folder = Self.incomingRoot.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let collected = Collected()
        let group = DispatchGroup()
        receiving = true
        publishState(.receiving(UInt32(session.items.count)))
        for (index, item) in session.items.enumerated() {
            let provider = item.itemProvider
            guard let type = fileType(of: provider) else {
                let accepted = provider.registeredTypeIdentifiers.compactMap(UTType.init)
                    .contains { $0.conforms(to: .data) && !$0.conforms(to: .text) }
                collected.skip(index, SkippedFile(
                    displayName: Self.safeName(provider.suggestedName, ext: nil),
                    reason: accepted ? .wrongType : .notAFile
                ))
                continue
            }
            group.enter()
            provider.loadFileRepresentation(forTypeIdentifier: type.identifier) { url, _ in
                defer { group.leave() }
                let name = Self.safeName(
                    provider.suggestedName ?? url?.deletingPathExtension().lastPathComponent,
                    ext: type.preferredFilenameExtension
                )
                // The file at `url` is deleted when this returns: copy now.
                guard let url = url else {
                    collected.skip(index, SkippedFile(displayName: name, reason: .unreadable))
                    return
                }
                let values = try? url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
                guard values?.isRegularFile == true else {
                    collected.skip(index, SkippedFile(displayName: name, reason: .notAFile))
                    return
                }
                if let size = values?.fileSize, UInt64(size) > limit {
                    collected.skip(index, SkippedFile(displayName: name, reason: .tooLarge))
                    return
                }
                // One folder per item: two items may carry the same name.
                let place = folder.appendingPathComponent("\(index)", isDirectory: true)
                do {
                    try FileManager.default.createDirectory(at: place, withIntermediateDirectories: true)
                    let target = place.appendingPathComponent(name)
                    try FileManager.default.copyItem(at: url, to: target)
                    // The size may be unknown before the copy.
                    let copied = (try? target.resourceValues(forKeys: [.fileSizeKey]))?.fileSize ?? 0
                    if UInt64(copied) > limit {
                        try? FileManager.default.removeItem(at: place)
                        collected.skip(index, SkippedFile(displayName: name, reason: .tooLarge))
                        return
                    }
                    collected.add(index, DroppedFile(
                        path: target.path,
                        displayName: name,
                        mimeType: type.preferredMIMEType,
                        size: UInt64(copied)
                    ))
                } catch {
                    try? FileManager.default.removeItem(at: place)
                    collected.skip(index, SkippedFile(displayName: name, reason: .unreadable))
                }
            }
        }
        group.notify(queue: .main) { [weak self, codecs] in
            let (files, skipped) = collected.take()
            if !files.isEmpty || !skipped.isEmpty {
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
            MainActor.assumeIsolated {
                self?.receiving = false
                self?.publishState(.idle)
            }
        }
    }
}

/// What the load callbacks of one drop gather, from any thread, kept in
/// drag order.
private final class Collected: @unchecked Sendable {
    private let lock = NSLock()
    private var files: [(Int, DroppedFile)] = []
    private var skipped: [(Int, SkippedFile)] = []

    func add(_ index: Int, _ file: DroppedFile) {
        lock.lock()
        files.append((index, file))
        lock.unlock()
    }

    func skip(_ index: Int, _ file: SkippedFile) {
        lock.lock()
        skipped.append((index, file))
        lock.unlock()
    }

    func take() -> ([DroppedFile], [SkippedFile]) {
        lock.lock()
        defer { lock.unlock() }
        return (
            files.sorted { $0.0 < $1.0 }.map(\.1),
            skipped.sorted { $0.0 < $1.0 }.map(\.1)
        )
    }
}
#endif
