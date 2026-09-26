import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif
#if canImport(LinkPresentation)
import LinkPresentation
#endif
#if canImport(UniformTypeIdentifiers)
import UniformTypeIdentifiers
#endif
#if canImport(Intents)
import Intents
#endif

/// Reference `UIActivityViewController` backend for `istmo.share`.
///
/// ```swift
/// let share = ShareBackendImpl()
/// IstmoRuntime.shared.registerHandler(
///     ShareDispatcher.PLUGIN_ID,
///     ShareDispatcher(backend: share, codecs: ShareCodecsImpl())
/// )
/// ```
///
/// In-memory files are written to `tmp/istmo-share/outgoing/` before
/// presenting; outgoing copies older than a day are pruned on every
/// share. On iPad the sheet is a popover anchored on
/// `ShareRequest.anchor.rect` (key-window points), or on the centre of
/// the key window when no anchor is given. Cancelling the Rust call
/// dismisses the sheet.
///
/// Direct-share targets are donated as `INSendMessageIntent`
/// interactions, which iOS shows in the share sheet's suggestion row.
/// That needs `INSendMessageIntent` in the app's `NSUserActivityTypes`
/// and a Share Extension that lists it under `IntentsSupported` (see
/// `templates/apple-share-extension`).
public final class ShareBackendImpl: ShareBackend {

    private let presenter: () -> AnyObject?
    private let lock = NSLock()
    private var busy = false

    /// - Parameter presenter: returns the view controller to present
    ///   from. Defaults to the top-most controller of the key window.
    public init(presenter: (() -> AnyObject?)? = nil) {
        #if canImport(UIKit)
        self.presenter = presenter ?? { ShareBackendImpl.topViewController() }
        #else
        self.presenter = presenter ?? { nil }
        #endif
    }

    /// Donation group, so replacing targets never touches the app's
    /// other donated interactions.
    private static let donationGroup = "dev.istmo.share.targets"

    public func capabilities() async throws -> ShareCapabilities {
        #if canImport(UIKit)
        ShareCapabilities(
            send: true,
            files: true,
            mixedContent: true,
            richPreview: true,
            reportsCompletion: true,
            reportsTarget: true,
            receive: IstmoShareHandoff.configuredAppGroup() != nil,
            directShare: Self.directShareConfigured(),
            dismissOnCancel: true
        )
        #else
        ShareCapabilities(
            send: false, files: false, mixedContent: false, richPreview: false,
            reportsCompletion: false, reportsTarget: false, receive: false,
            directShare: false, dismissOnCancel: false
        )
        #endif
    }

    public func staging_dir() async throws -> String {
        let root = OutgoingStaging().root
        do {
            try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        } catch {
            throw ShareError.io("\(root.path): \(error.localizedDescription)")
        }
        return root.path
    }

    public func set_share_targets(targets: [ShareTarget]) async throws {
        #if canImport(Intents) && canImport(UIKit)
        guard Self.directShareConfigured() else {
            throw ShareError.unsupported("add INSendMessageIntent to NSUserActivityTypes to donate share targets")
        }
        do {
            try await INInteraction.delete(with: Self.donationGroup)
            for target in targets {
                let interaction = INInteraction(intent: try Self.messageIntent(for: target), response: nil)
                interaction.direction = .outgoing
                interaction.groupIdentifier = Self.donationGroup
                try await interaction.donate()
            }
        } catch let error as ShareError {
            throw error
        } catch {
            throw ShareError.backend(error.localizedDescription)
        }
        #else
        throw ShareError.unsupported("share-sheet suggestions need UIKit and Intents")
        #endif
    }

    private static func directShareConfigured() -> Bool {
        let types = Bundle.main.object(forInfoDictionaryKey: "NSUserActivityTypes") as? [String] ?? []
        return types.contains("INSendMessageIntent")
    }

    #if canImport(Intents) && canImport(UIKit)
    private static func messageIntent(for target: ShareTarget) throws -> INSendMessageIntent {
        let name = INSpeakableString(spokenPhrase: target.label)
        let intent: INSendMessageIntent
        if #available(iOS 14.0, *) {
            intent = INSendMessageIntent(
                recipients: nil, outgoingMessageType: .outgoingMessageText, content: nil,
                speakableGroupName: name, conversationIdentifier: target.id,
                serviceName: nil, sender: nil, attachments: nil
            )
        } else {
            intent = INSendMessageIntent(
                recipients: nil, content: nil, speakableGroupName: name,
                conversationIdentifier: target.id, serviceName: nil, sender: nil
            )
        }
        if let icon = target.icon {
            let data: Data
            switch icon.source {
            case .path(let path):
                guard let loaded = FileManager.default.contents(atPath: path) else {
                    throw ShareError.notFound(path)
                }
                data = loaded
            case .bytes(let bytes):
                data = bytes
            }
            intent.setImage(INImage(imageData: data), forParameterNamed: \.speakableGroupName)
        }
        return intent
    }
    #endif

    public func share(request: ShareRequest) async throws -> ShareOutcome {
        #if canImport(UIKit)
        guard request.text != nil || request.url != nil || !request.files.isEmpty else {
            throw ShareError.invalidRequest("share request has no text, url or files")
        }
        try acquire()
        defer { release() }

        let staging = OutgoingStaging()
        staging.prune()
        let fileURLs = try request.files.map { try staging.stage($0) }
        let thumbnailURL = try request.preview?.thumbnail.map { try staging.stage($0) }
        let metadata = await linkMetadata(request: request, thumbnailURL: thumbnailURL)

        var items: [Any] = []
        if let text = request.text {
            items.append(TextItemSource(text: text, subject: request.subject, metadata: request.url == nil ? metadata : nil))
        }
        if let urlString = request.url, let url = URL(string: urlString) {
            items.append(URLItemSource(url: url, subject: request.subject, metadata: metadata))
        } else if let urlString = request.url {
            throw ShareError.invalidRequest("not a valid URL: \(urlString)")
        }
        items.append(contentsOf: fileURLs)

        return try await present(items: items, anchor: request.anchor)
        #else
        throw ShareError.unsupported("UIKit unavailable")
        #endif
    }

    private func acquire() throws {
        lock.lock()
        defer { lock.unlock() }
        if busy { throw ShareError.busy }
        busy = true
    }

    private func release() {
        lock.lock()
        busy = false
        lock.unlock()
    }

    #if canImport(UIKit)
    @MainActor
    private func present(items: [Any], anchor: ShareAnchor?) async throws -> ShareOutcome {
        guard let host = presenter() as? UIViewController else {
            throw ShareError.noPresenter("no view controller to present UIActivityViewController from")
        }
        let controller = UIActivityViewController(activityItems: items, applicationActivities: nil)
        let once = OnceContinuation()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { cont in
                once.install(cont)
                if Task.isCancelled {
                    once.resume(.success(.dismissed))
                    return
                }
                controller.completionWithItemsHandler = { activityType, completed, _, error in
                    if let error = error {
                        once.resume(.failure(ShareError.backend(error.localizedDescription)))
                    } else if completed {
                        once.resume(.success(.shared(activityType?.rawValue)))
                    } else {
                        once.resume(.success(.dismissed))
                    }
                }
                if let popover = controller.popoverPresentationController {
                    let view: UIView = host.view.window ?? host.view
                    popover.sourceView = view
                    if let rect = anchor?.rect {
                        popover.sourceRect = CGRect(x: rect.x, y: rect.y, width: rect.width, height: rect.height)
                    } else {
                        popover.sourceRect = CGRect(x: view.bounds.midX, y: view.bounds.midY, width: 0, height: 0)
                        popover.permittedArrowDirections = []
                    }
                }
                host.present(controller, animated: true)
            }
        } onCancel: {
            // The Rust caller dropped the share: close the sheet. A
            // programmatic dismissal does not fire the completion handler.
            DispatchQueue.main.async {
                controller.dismiss(animated: true)
                once.resume(.success(.dismissed))
            }
        }
    }

    private func linkMetadata(request: ShareRequest, thumbnailURL: URL?) async -> LPLinkMetadata? {
        guard let preview = request.preview else { return nil }
        var metadata = LPLinkMetadata()
        if preview.fetchLinkMetadata, let urlString = request.url, let url = URL(string: urlString) {
            if let fetched = try? await LPMetadataProvider().startFetchingMetadata(for: url) {
                metadata = fetched
            }
        }
        if let urlString = request.url, let url = URL(string: urlString) {
            metadata.originalURL = url
            metadata.url = metadata.url ?? url
        }
        if let title = preview.title ?? request.subject {
            metadata.title = title
        }
        if let thumbnailURL = thumbnailURL, let provider = NSItemProvider(contentsOf: thumbnailURL) {
            metadata.imageProvider = provider
            metadata.iconProvider = provider
        }
        return metadata
    }

    static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        let window = scenes.flatMap(\.windows).first(where: \.isKeyWindow) ?? scenes.first?.windows.first
        var top = window?.rootViewController
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
    }
    #endif
}

#if canImport(UIKit)
/// Resumes a continuation exactly once — the sheet's completion handler
/// and a cancellation can race.
final class OnceContinuation: @unchecked Sendable {
    private let lock = NSLock()
    private var continuation: CheckedContinuation<ShareOutcome, Error>?

    func install(_ continuation: CheckedContinuation<ShareOutcome, Error>) {
        lock.lock()
        self.continuation = continuation
        lock.unlock()
    }

    func resume(_ result: Result<ShareOutcome, Error>) {
        lock.lock()
        let continuation = self.continuation
        self.continuation = nil
        lock.unlock()
        continuation?.resume(with: result)
    }
}

/// Plain-text item that also feeds the mail subject and, when no link is
/// shared, the rich preview header.
final class TextItemSource: NSObject, UIActivityItemSource {
    private let text: String
    private let subject: String?
    private let metadata: LPLinkMetadata?

    init(text: String, subject: String?, metadata: LPLinkMetadata?) {
        self.text = text
        self.subject = subject
        self.metadata = metadata
    }

    func activityViewControllerPlaceholderItem(_ controller: UIActivityViewController) -> Any { text }

    func activityViewController(_ controller: UIActivityViewController, itemForActivityType type: UIActivity.ActivityType?) -> Any? { text }

    func activityViewController(_ controller: UIActivityViewController, subjectForActivityType type: UIActivity.ActivityType?) -> String {
        subject ?? ""
    }

    func activityViewControllerLinkMetadata(_ controller: UIActivityViewController) -> LPLinkMetadata? { metadata }
}

/// Link item carrying the optional `LPLinkMetadata` preview.
final class URLItemSource: NSObject, UIActivityItemSource {
    private let url: URL
    private let subject: String?
    private let metadata: LPLinkMetadata?

    init(url: URL, subject: String?, metadata: LPLinkMetadata?) {
        self.url = url
        self.subject = subject
        self.metadata = metadata
    }

    func activityViewControllerPlaceholderItem(_ controller: UIActivityViewController) -> Any { url }

    func activityViewController(_ controller: UIActivityViewController, itemForActivityType type: UIActivity.ActivityType?) -> Any? { url }

    func activityViewController(_ controller: UIActivityViewController, subjectForActivityType type: UIActivity.ActivityType?) -> String {
        subject ?? ""
    }

    func activityViewControllerLinkMetadata(_ controller: UIActivityViewController) -> LPLinkMetadata? { metadata }
}
#endif

/// Writes outgoing files under `tmp/istmo-share/outgoing/<uuid>/`.
struct OutgoingStaging {
    private static let ttl: TimeInterval = 24 * 60 * 60
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("istmo-share/outgoing", isDirectory: true)

    func stage(_ file: ShareFile) throws -> URL {
        switch file.source {
        case .path(let path):
            guard FileManager.default.fileExists(atPath: path) else {
                throw ShareError.notFound(path)
            }
            let url = URL(fileURLWithPath: path)
            // A display name different from the on-disk name needs a
            // renamed copy — receivers show the file name.
            guard let name = file.name, name != url.lastPathComponent else { return url }
            return try write(name: name) { dest in try FileManager.default.copyItem(at: url, to: dest) }
        case .bytes(let data):
            return try write(name: file.name ?? "shared") { dest in try data.write(to: dest) }
        }
    }

    private func write(name: String, _ body: (URL) throws -> Void) throws -> URL {
        let dir = root.appendingPathComponent(UUID().uuidString, isDirectory: true)
        let dest = dir.appendingPathComponent(Self.sanitize(name))
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            try body(dest)
        } catch {
            throw ShareError.io("\(dest.path): \(error.localizedDescription)")
        }
        return dest
    }

    func prune() {
        let fm = FileManager.default
        guard let entries = try? fm.contentsOfDirectory(at: root, includingPropertiesForKeys: [.contentModificationDateKey]) else {
            return
        }
        let cutoff = Date().addingTimeInterval(-Self.ttl)
        for entry in entries {
            let modified = (try? entry.resourceValues(forKeys: [.contentModificationDateKey]))?.contentModificationDate
            if let modified = modified, modified < cutoff {
                try? fm.removeItem(at: entry)
            }
        }
    }

    static func sanitize(_ name: String) -> String {
        let base = (name as NSString).lastPathComponent
        let forbidden = CharacterSet(charactersIn: "/\\:*?\"<>|").union(.controlCharacters)
        let cleaned = String(base.unicodeScalars.map { forbidden.contains($0) ? "_" : Character($0) })
        return cleaned.trimmingCharacters(in: CharacterSet(charactersIn: ".")).isEmpty ? "shared" : cleaned
    }
}
