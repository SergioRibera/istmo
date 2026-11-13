import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
import UniformTypeIdentifiers
#endif

/// Reference `UIDocumentPickerViewController` backend for
/// `istmo.file_picker`.
///
/// The generated `IstmoPluginRegistry.registerAll()` registers it.
/// Registering by hand wraps it in the generated dispatcher:
///
/// ```swift
/// IstmoRuntime.shared.registerHandler(
///     FilePickerDispatcher.PLUGIN_ID,
///     FilePickerDispatcher(backend: FilePickerBackendImpl(), codecs: FilePickerCodecsImpl())
/// )
/// ```
///
/// The backend serialises picker launches — `UIDocumentPickerViewController`
/// only tolerates one modal at a time.
public final class FilePickerBackendImpl: FilePickerBackend, HandleReleaser {

    #if canImport(UIKit)
    private struct Entry {
        let url: URL
        let scopeActive: Bool
    }

    private let queue = DispatchQueue(label: "dev.istmo.plugins.file-picker.backend")
    private var files: [UInt64: Entry] = [:]
    private let launchGate = LaunchGate()

    public init() {}

    // ------------------------------------------------------- FilePicker impl

    public func pick_file(config: PickConfig) async throws -> PickedFile? {
        let urls = try await present(config: config, multi: false, save: false, saveConfig: nil)
        guard let url = urls.first else { return nil }
        return register(url: url)
    }

    public func pick_files(config: PickConfig) async throws -> [PickedFile] {
        let urls = try await present(config: config, multi: config.allowMultiple, save: false, saveConfig: nil)
        return urls.map(register(url:))
    }

    public func save_file(config: SaveConfig) async throws -> PickedFile? {
        let urls = try await present(config: nil, multi: false, save: true, saveConfig: config)
        guard let url = urls.first else { return nil }
        return register(url: url)
    }

    public func open_read(file: NativeHandleId) async throws -> RawFileHandle {
        let entry = try lookup(file)
        let fd = Darwin.open(entry.url.path, O_RDONLY)
        if fd < 0 {
            throw FilePickerError.io("open(\(entry.url.path), O_RDONLY) failed: errno \(errno)")
        }
        return RawFileHandle(raw: Int64(fd))
    }

    public func open_write(file: NativeHandleId) async throws -> RawFileHandle {
        let entry = try lookup(file)
        let fd = Darwin.open(entry.url.path, O_WRONLY | O_TRUNC)
        if fd < 0 {
            throw FilePickerError.io("open(\(entry.url.path), O_WRONLY) failed: errno \(errno)")
        }
        return RawFileHandle(raw: Int64(fd))
    }

    public func path(file: NativeHandleId) async throws -> String? {
        let entry = try lookup(file)
        return entry.url.path
    }

    // ------------------------------------------------------- HandleReleaser

    public func releaseNativeHandle(_ handleId: UInt64) {
        let entry: Entry? = queue.sync { files.removeValue(forKey: handleId) }
        guard let entry else { return }
        if entry.scopeActive {
            entry.url.stopAccessingSecurityScopedResource()
        }
    }

    // ------------------------------------------------------- helpers

    private func lookup(_ id: NativeHandleId) throws -> Entry {
        let entry: Entry? = queue.sync { files[id] }
        guard let entry else {
            throw FilePickerError.notFound("handle \(id)")
        }
        return entry
    }

    private func register(url: URL) -> PickedFile {
        let scopeActive = url.startAccessingSecurityScopedResource()
        let id: NativeHandleId = IstmoRuntime.shared.allocHandleId(pluginId: "istmo.file_picker")
        queue.sync {
            files[id] = Entry(url: url, scopeActive: scopeActive)
        }
        let (name, size, mime) = describe(url: url)
        return PickedFile(displayName: name, mimeType: mime, size: size, handle: id)
    }

    private func describe(url: URL) -> (String, UInt64?, String?) {
        let name = url.lastPathComponent
        let attrs = try? FileManager.default.attributesOfItem(atPath: url.path)
        let size = (attrs?[.size] as? NSNumber)?.uint64Value
        var mime: String?
        if #available(iOS 14.0, *) {
            mime = UTType(filenameExtension: url.pathExtension)?.preferredMIMEType
        }
        return (name, size, mime)
    }

    private func present(
        config: PickConfig?,
        multi: Bool,
        save: Bool,
        saveConfig: SaveConfig?
    ) async throws -> [URL] {
        // An actor gate rather than a semaphore: blocking a cooperative
        // thread inside `async` code can starve the Swift concurrency pool.
        await launchGate.acquire()
        do {
            let urls = try await presentLocked(config: config, multi: multi, save: save, saveConfig: saveConfig)
            await launchGate.release()
            return urls
        } catch {
            await launchGate.release()
            throw error
        }
    }

    private func presentLocked(
        config: PickConfig?,
        multi: Bool,
        save: Bool,
        saveConfig: SaveConfig?
    ) async throws -> [URL] {
        // Save flow: an empty placeholder, named after the sanitised
        // suggestion, inside a temp directory of its own. Exporting it
        // with `asCopy: false` *moves* it to the location the user picks
        // and reports that destination URL — security-scoped and
        // writable — which `open_write` then targets. A copy would leave
        // us holding only the placeholder.
        let export: (dir: URL, file: URL)? = try save
            ? Self.makeSavePlaceholder(suggestedName: saveConfig?.suggestedName)
            : nil
        defer {
            if let export {
                try? FileManager.default.removeItem(at: export.dir)
            }
        }
        let placeholder = export?.file

        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<[URL], Error>) in
            DispatchQueue.main.async {
                guard let root = Self.topViewController() else {
                    cont.resume(throwing: FilePickerError.backend("no root view controller to present from"))
                    return
                }
                let picker: UIDocumentPickerViewController
                if let placeholder {
                    if #available(iOS 14.0, *) {
                        picker = UIDocumentPickerViewController(forExporting: [placeholder], asCopy: false)
                    } else {
                        picker = UIDocumentPickerViewController(url: placeholder, in: .moveToService)
                    }
                } else if #available(iOS 14.0, *) {
                    let types = Self.utTypes(from: config?.filter)
                    picker = UIDocumentPickerViewController(forOpeningContentTypes: types, asCopy: false)
                    picker.allowsMultipleSelection = multi
                } else {
                    picker = UIDocumentPickerViewController(documentTypes: ["public.item"], in: .open)
                    picker.allowsMultipleSelection = multi
                }
                let delegate = FilePickerDelegate(continuation: cont)
                picker.delegate = delegate
                objc_setAssociatedObject(picker, &FilePickerDelegate.assocKey, delegate, .OBJC_ASSOCIATION_RETAIN)
                root.present(picker, animated: true)
            }
        }
    }

    private static func makeSavePlaceholder(suggestedName: String?) throws -> (dir: URL, file: URL) {
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("istmo-file-picker-\(UUID().uuidString)", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: false)
            let file = dir.appendingPathComponent(sanitizedFileName(suggestedName))
            try Data().write(to: file)
            return (dir, file)
        } catch {
            try? FileManager.default.removeItem(at: dir)
            throw FilePickerError.io("create save placeholder: \(error)")
        }
    }

    /// Last path component of `suggested`, so a name like `../../x` or
    /// `a/b` cannot point the placeholder outside its temp directory.
    private static func sanitizedFileName(_ suggested: String?) -> String {
        let cleaned = (suggested ?? "")
            .replacingOccurrences(of: "\0", with: "")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: ":", with: "_")
        // `URL(fileURLWithPath: "")` is the working directory, whose name
        // would otherwise become the suggestion.
        if cleaned.isEmpty {
            return "untitled"
        }
        let name = URL(fileURLWithPath: cleaned).lastPathComponent
        if name.isEmpty || name == "." || name == ".." || name == "/" {
            return "untitled"
        }
        return name
    }

    @available(iOS 14.0, *)
    private static func utTypes(from filter: FileFilter?) -> [UTType] {
        guard let filter else { return [.item] }
        var out: [UTType] = []
        out.append(contentsOf: filter.utiTypes.compactMap(UTType.init(_:)))
        out.append(contentsOf: filter.mimeTypes.compactMap(UTType.init(mimeType:)))
        out.append(contentsOf: filter.extensions.compactMap(UTType.init(filenameExtension:)))
        return out.isEmpty ? [.item] : out
    }

    private static func topViewController() -> UIViewController? {
        // `UIWindowScene.keyWindow` is iOS 15+; `isKeyWindow` keeps iOS 13.
        var top: UIViewController? = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap(\.windows)
            .first(where: \.isKeyWindow)?
            .rootViewController
        while let presented = top?.presentedViewController {
            top = presented
        }
        return top
    }

    #else
    public init() {}
    public func pick_file(config: PickConfig) async throws -> PickedFile? {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func pick_files(config: PickConfig) async throws -> [PickedFile] {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func save_file(config: SaveConfig) async throws -> PickedFile? {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func open_read(file: NativeHandleId) async throws -> RawFileHandle {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func open_write(file: NativeHandleId) async throws -> RawFileHandle {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func path(file: NativeHandleId) async throws -> String? {
        throw FilePickerError.unsupportedOperation("file-picker requires UIKit")
    }
    public func releaseNativeHandle(_ handleId: UInt64) {}
    #endif
}

/// One picker at a time; waiters resume in FIFO order.
private actor LaunchGate {
    private var busy = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func acquire() async {
        if !busy {
            busy = true
            return
        }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        if waiters.isEmpty {
            busy = false
        } else {
            // Ownership passes straight to the next waiter; `busy` stays set.
            waiters.removeFirst().resume()
        }
    }
}

#if canImport(UIKit)
private final class FilePickerDelegate: NSObject, UIDocumentPickerDelegate {
    static var assocKey: UInt8 = 0
    private let continuation: CheckedContinuation<[URL], Error>
    private var resumed = false

    init(continuation: CheckedContinuation<[URL], Error>) {
        self.continuation = continuation
    }

    func documentPicker(_ controller: UIDocumentPickerViewController, didPickDocumentsAt urls: [URL]) {
        guard !resumed else { return }
        resumed = true
        continuation.resume(returning: urls)
    }

    func documentPickerWasCancelled(_ controller: UIDocumentPickerViewController) {
        guard !resumed else { return }
        resumed = true
        continuation.resume(returning: [])
    }
}
#endif
