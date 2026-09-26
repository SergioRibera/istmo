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
#if canImport(Intents)
import Intents
#endif
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
            #if canImport(Intents) && canImport(UIKit)
            // Set when the user picked one of the app's donated
            // share-sheet suggestions (ShareClient::set_share_targets).
            manifest.targetId = (context.intent as? INSendMessageIntent)?.conversationIdentifier
            #endif
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
            IstmoShareHandoff.postHandoff(appGroup: group)
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

    /// Bring the containing app up so it drains the inbox now. The
    /// Darwin notification posted before this already covers an app
    /// whose process is alive; this handles one that is not running.
    ///
    /// iOS: extensions may not call `UIApplication.open` (it does not
    /// compile in an extension target), and the old
    /// `perform("openURL:")` trick stopped working in iOS 18. Walking the
    /// responder chain to the application object and invoking
    /// `openURL:options:completionHandler:` through its implementation
    /// still works. When it fails, the app drains on its next
    /// activation.
    ///
    /// macOS: launch the containing app in the background if it is not
    /// running; it drains at startup.
    private func wakeContainingApp() {
        #if canImport(UIKit)
        guard let scheme = Bundle.main.object(forInfoDictionaryKey: IstmoShareHandoff.urlSchemeInfoKey) as? String,
              !scheme.isEmpty,
              let url = URL(string: "\(scheme)://\(IstmoShareHandoff.wakeHost)") else {
            return
        }
        openThroughResponderChain(url)
        #else
        // .../MyApp.app/Contents/PlugIns/Share.appex → .../MyApp.app
        let appURL = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
        guard let bundleId = Bundle(url: appURL)?.bundleIdentifier,
              NSRunningApplication.runningApplications(withBundleIdentifier: bundleId).isEmpty else {
            return
        }
        let configuration = NSWorkspace.OpenConfiguration()
        configuration.activates = false
        NSWorkspace.shared.openApplication(at: appURL, configuration: configuration)
        #endif
    }

    #if canImport(UIKit)
    private func openThroughResponderChain(_ url: URL) {
        typealias OpenURL = @convention(c) (AnyObject, Selector, NSURL, NSDictionary, AnyObject?) -> Void
        let selector = NSSelectorFromString("openURL:options:completionHandler:")
        var responder: UIResponder? = self
        while let current = responder {
            if current is UIApplication, current.responds(to: selector) {
                let open = unsafeBitCast(current.method(for: selector), to: OpenURL.self)
                open(current, selector, url as NSURL, NSDictionary(), nil)
                return
            }
            responder = current.next
        }
    }
    #endif
}
