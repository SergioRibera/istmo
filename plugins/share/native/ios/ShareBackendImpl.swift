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
/// the key window when no anchor is given.
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

    public func capabilities() async throws -> ShareCapabilities {
        #if canImport(UIKit)
        ShareCapabilities(
            send: true,
            files: true,
            mixedContent: true,
            richPreview: true,
            reportsCompletion: true,
            reportsTarget: true,
            receive: true
        )
        #else
        ShareCapabilities(
            send: false, files: false, mixedContent: false, richPreview: false,
            reportsCompletion: false, reportsTarget: false, receive: false
        )
        #endif
    }

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
        return try await withCheckedThrowingContinuation { cont in
            let controller = UIActivityViewController(activityItems: items, applicationActivities: nil)
            controller.completionWithItemsHandler = { activityType, completed, _, error in
                if let error = error {
                    cont.resume(throwing: ShareError.backend(error.localizedDescription))
                } else if completed {
                    cont.resume(returning: .shared(activityType?.rawValue))
                } else {
                    cont.resume(returning: .dismissed)
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
