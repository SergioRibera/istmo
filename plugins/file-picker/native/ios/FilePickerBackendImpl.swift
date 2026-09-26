import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
import UniformTypeIdentifiers
#endif

/// Reference `UIDocumentPickerViewController` backend for
/// `istmo.file_picker`.
///
/// Construct after your app finishes launching and register with the
/// plugin registry:
///
/// ```swift
/// let picker = FilePickerBackendImpl()
/// IstmoRuntime.shared.registerHandler("istmo.file_picker", picker)
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
    private let launchGate = DispatchSemaphore(value: 1)

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
        launchGate.wait()
        defer { launchGate.signal() }

        return try await withCheckedThrowingContinuation { (cont: CheckedContinuation<[URL], Error>) in
            DispatchQueue.main.async {
                guard let root = Self.topViewController() else {
                    cont.resume(throwing: FilePickerError.backend("no root view controller to present from"))
                    return
                }
                let picker: UIDocumentPickerViewController
                if save, let saveConfig {
                    // Save picker: write suggestedName to a temp URL first so we
                    // can hand it to `forExporting:` and get "Save As" semantics.
                    let name = saveConfig.suggestedName ?? "untitled"
                    let tmp = FileManager.default.temporaryDirectory.appendingPathComponent(name)
                    try? Data().write(to: tmp)
                    if #available(iOS 14.0, *) {
                        picker = UIDocumentPickerViewController(forExporting: [tmp], asCopy: true)
                    } else {
                        picker = UIDocumentPickerViewController(url: tmp, in: .exportToService)
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
        var top: UIViewController? = UIApplication.shared.connectedScenes
            .compactMap { ($0 as? UIWindowScene)?.keyWindow?.rootViewController }
            .first
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
