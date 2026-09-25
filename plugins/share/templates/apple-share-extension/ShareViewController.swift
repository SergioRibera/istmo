// istmo.share — Share Extension template (iOS and macOS).
//
// Copy this directory into your Xcode project as a Share Extension
// target (see README.md). The extension copies whatever was shared
// into the App Group inbox, wakes the app (iOS) and closes; the app
// publishes it to Rust with `IstmoShareInbox.drain()` (iOS) or
// `istmo_share::desktop::drain_app_group_inbox` (macOS).
//
// Compile together with `native/ios/ShareHandoff.swift` from the
// istmo-share crate.

import Foundation
import UniformTypeIdentifiers
#if canImport(UIKit)
import UIKit
typealias PlatformViewController = UIViewController
#else
import AppKit
typealias PlatformViewController = NSViewController
#endif

final class ShareViewController: PlatformViewController {

    #if canImport(AppKit) && !canImport(UIKit)
    override func loadView() {
        view = NSView(frame: .zero)
    }
    #endif

    #if canImport(UIKit)
    override func viewDidAppear(_ animated: Bool) {
        super.viewDidAppear(animated)
        Task { await handle() }
    }
    #else
    override func viewDidAppear() {
        super.viewDidAppear()
        Task { await handle() }
    }
    #endif

    private func handle() async {
        guard let context = extensionContext else { return }
        do {
            guard let group = IstmoShareHandoff.configuredAppGroup() else {
                throw CocoaError(.featureUnsupported, userInfo: [
                    NSLocalizedDescriptionKey: "\(IstmoShareHandoff.appGroupInfoKey) missing from the extension Info.plist",
                ])
            }
            let entry = try IstmoShareHandoff.beginEntry(appGroup: group)
            var manifest = IstmoShareHandoff.Manifest(receivedAtMs: IstmoShareHandoff.nowMs())
            let items = context.inputItems.compactMap { $0 as? NSExtensionItem }
            for item in items {
                if manifest.subject == nil {
                    manifest.subject = item.attributedTitle?.string
                }
                if manifest.text == nil, let body = item.attributedContentText?.string, !body.isEmpty {
                    manifest.text = body
                }
                for provider in item.attachments ?? [] {
                    try await collect(provider, into: &manifest, entry: entry)
                }
            }
            try IstmoShareHandoff.commit(entry: entry, manifest: manifest)
            wakeContainingApp()
            context.completeRequest(returningItems: nil)
        } catch {
            context.cancelRequest(withError: error)
        }
    }

    private func collect(
        _ provider: NSItemProvider,
        into manifest: inout IstmoShareHandoff.Manifest,
        entry: URL
    ) async throws {
        let has = { (type: UTType) in provider.hasItemConformingToTypeIdentifier(type.identifier) }
        let isFile = [UTType.fileURL, .image, .movie, .pdf].contains(where: has)
        let isOpaqueData = has(.data) && !has(.url) && !has(.plainText)
        if isFile || isOpaqueData {
            let type = provider.registeredTypeIdentifiers.first ?? UTType.data.identifier
            if let file = try await copyFile(provider, type: type, entry: entry) {
                manifest.files.append(file)
            }
        } else if provider.hasItemConformingToTypeIdentifier(UTType.url.identifier) {
            let item = try await provider.loadItem(forTypeIdentifier: UTType.url.identifier)
            if let url = item as? URL {
                manifest.url = url.absoluteString
            }
        } else if provider.hasItemConformingToTypeIdentifier(UTType.plainText.identifier) {
            let item = try await provider.loadItem(forTypeIdentifier: UTType.plainText.identifier)
            if let text = item as? String {
                manifest.text = [manifest.text, text].compactMap { $0 }.joined(separator: "\n")
            }
        }
    }

    private func copyFile(
        _ provider: NSItemProvider,
        type: String,
        entry: URL
    ) async throws -> IstmoShareHandoff.File? {
        try await withCheckedThrowingContinuation { cont in
            // The URL handed to the completion handler is only valid
            // inside it, so copy synchronously there.
            _ = provider.loadFileRepresentation(forTypeIdentifier: type) { url, error in
                if let error = error {
                    cont.resume(throwing: error)
                    return
                }
                guard let url = url else {
                    cont.resume(returning: nil)
                    return
                }
                let display = provider.suggestedName.map { name -> String in
                    url.pathExtension.isEmpty || !(name as NSString).pathExtension.isEmpty
                        ? name : "\(name).\(url.pathExtension)"
                } ?? url.lastPathComponent
                let fileName = IstmoShareHandoff.fileName(for: display, in: entry)
                do {
                    try FileManager.default.copyItem(at: url, to: entry.appendingPathComponent(fileName))
                    let mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
                        ?? UTType(type)?.preferredMIMEType
                    cont.resume(returning: IstmoShareHandoff.File(file: fileName, name: display, mimeType: mime))
                } catch {
                    cont.resume(throwing: error)
                }
            }
        }
    }

    /// iOS: open `<scheme>://istmo-share` so the app drains the inbox
    /// right away. Share extensions cannot call `UIApplication.open`
    /// directly; walking the responder chain to the shared application
    /// is the established workaround. When it fails, the app still
    /// drains on its next activation.
    private func wakeContainingApp() {
        #if canImport(UIKit)
        guard let scheme = Bundle.main.object(forInfoDictionaryKey: IstmoShareHandoff.urlSchemeInfoKey) as? String,
              !scheme.isEmpty,
              let url = URL(string: "\(scheme)://\(IstmoShareHandoff.wakeHost)") else {
            return
        }
        var responder: UIResponder? = self
        while let current = responder {
            if let application = current as? UIApplication {
                application.open(url, options: [:], completionHandler: nil)
                return
            }
            responder = current.next
        }
        #endif
    }
}
