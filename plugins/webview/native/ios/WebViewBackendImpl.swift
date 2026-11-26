import Foundation
import IstmoRuntime
#if canImport(WebKit) && canImport(UIKit)
import UIKit
import WebKit
#endif

/// `istmo.webview` on `WKWebView`.
///
/// Embedded views are subviews laid over the app's root view (frames
/// arrive in physical pixels and are applied once per run-loop turn);
/// windowed views are presented view controllers. Every engine call runs
/// on the main thread.
public final class WebViewBackendImpl: NSObject, WebViewBackend, HandleReleaser {
    let config: WebView.WebViewInitConfig
    private let lock = NSLock()
    private var views: [UInt64: AnyObject] = [:]

    public init(config: WebView.WebViewInitConfig) {
        self.config = config
        super.init()
    }

    // MARK: table (synchronous: `NSLock` is unavailable in async contexts)

    func store(_ id: UInt64, _ view: AnyObject) {
        lock.lock()
        views[id] = view
        lock.unlock()
    }

    func take(_ id: UInt64) -> AnyObject? {
        lock.lock()
        defer { lock.unlock() }
        return views.removeValue(forKey: id)
    }

    #if canImport(WebKit) && canImport(UIKit)
    func allViews() -> [ManagedWebView] {
        lock.lock()
        defer { lock.unlock() }
        return views.values.compactMap { $0 as? ManagedWebView }
    }
    #endif

    func lookup(_ id: UInt64) -> AnyObject? {
        lock.lock()
        defer { lock.unlock() }
        return views[id]
    }

    // MARK: main-thread hops without `Sendable` requirements

    func onMain<T>(_ body: @escaping () throws -> T) async throws -> T {
        try await withCheckedThrowingContinuation { cont in
            DispatchQueue.main.async {
                do { cont.resume(returning: try body()) } catch { cont.resume(throwing: error) }
            }
        }
    }

    func onMainAsync<T>(_ body: @escaping (@escaping (Result<T, Error>) -> Void) -> Void) async throws -> T {
        try await withCheckedThrowingContinuation { cont in
            DispatchQueue.main.async {
                body { cont.resume(with: $0) }
            }
        }
    }

    public func releaseNativeHandle(_ handleId: UInt64) {
        guard lookup(handleId) != nil else { return }
        DispatchQueue.main.async { [weak self] in self?.remove(handleId) }
    }

    func remove(_ id: UInt64) {
        #if canImport(WebKit) && canImport(UIKit)
        (take(id) as? ManagedWebView)?.close()
        #else
        _ = take(id)
        #endif
        IstmoRuntime.shared.forgetHandle(id)
    }

    #if canImport(WebKit) && canImport(UIKit)
    func view(_ ref: WebView.ViewRef) throws -> ManagedWebView {
        guard let view = lookup(ref.view) as? ManagedWebView else { throw WebView.WebViewError.viewClosed }
        // Constant-time token comparison.
        guard (view.tokenHi ^ ref.tokenHi) | (view.tokenLo ^ ref.tokenLo) == 0 else {
            throw WebView.WebViewError.invalidToken
        }
        return view
    }

    lazy var schemeRouter = SchemeRouter(backend: self)
    #endif

    // MARK: WebViewBackend

    public func capabilities() async throws -> WebView.WebViewCapabilities {
        var persistent = false
        var inspector = false
        if #available(iOS 17.0, *) { persistent = true }
        if #available(iOS 16.4, *) { inspector = config.allowInspector }
        #if canImport(WebKit) && canImport(UIKit)
        let available = true
        #else
        let available = false
        #endif
        return WebView.WebViewCapabilities(
            backend: available ? "wkwebview" : "unavailable",
            engineVersion: "",
            embedded: available,
            windowed: available,
            windowParenting: false,
            persistentProfiles: available && persistent,
            ephemeralProfiles: available,
            isolatedWorld: available,
            bridgeReply: available,
            deferredNavigation: available,
            schemeRequestBodies: available,
            downloads: available,
            downloadControl: false,
            find: available,
            print: available,
            snapshot: available,
            inspector: inspector,
            schemeOrigins: config.schemes.map {
                WebView.SchemeOrigin(scheme: $0.name, origin: "\($0.name)://localhost", baseUrl: "\($0.name)://localhost/")
            }
        )
    }

    public func prewarm() async throws {
        #if canImport(WebKit) && canImport(UIKit)
        try await onMain {
            // Spawns the web content process ahead of the first view.
            _ = WKWebView(frame: .zero)
        }
        #endif
    }

    public func `open`(spec: WebView.WebViewSpec) async throws -> WebView.WebViewInstance {
        #if canImport(WebKit) && canImport(UIKit)
        return try await onMain { try self.create(spec: spec, configuration: nil).instance }
        #else
        throw WebView.WebViewError.unsupported("WebKit")
        #endif
    }

    #if canImport(WebKit) && canImport(UIKit)
    func create(spec: WebView.WebViewSpec, configuration: WKWebViewConfiguration?) throws -> ManagedWebView {
        let id = IstmoRuntime.shared.allocHandleId(pluginId: "istmo.webview")
        var generator = SystemRandomNumberGenerator()
        let view = try ManagedWebView(
            backend: self, id: id, tokenHi: generator.next(), tokenLo: generator.next(),
            spec: spec, inherited: configuration)
        store(id, view)
        return view
    }
    #endif

    public func close(view: WebView.ViewRef) async throws {
        #if canImport(WebKit) && canImport(UIKit)
        try await onMain {
            _ = try self.view(view)
            self.remove(view.view)
        }
        #else
        throw WebView.WebViewError.viewClosed
        #endif
    }

    #if canImport(WebKit) && canImport(UIKit)
    private func with<T>(_ ref: WebView.ViewRef, _ body: @escaping (ManagedWebView) throws -> T) async throws -> T {
        try await onMain { try body(try self.view(ref)) }
    }

    private func withAsync<T>(_ ref: WebView.ViewRef, _ body: @escaping (ManagedWebView, @escaping (Result<T, Error>) -> Void) -> Void) async throws -> T {
        try await onMainAsync { done in
            do { body(try self.view(ref), done) } catch { done(.failure(error)) }
        }
    }

    public func set_frame(view: WebView.ViewRef, frame: WebView.ViewFrame) async throws { try await with(view) { $0.setFrame(frame) } }
    public func focus(view: WebView.ViewRef) async throws { try await with(view) { _ = $0.webView.becomeFirstResponder() } }
    public func load(view: WebView.ViewRef, request: WebView.LoadRequest) async throws { try await with(view) { $0.load(request) } }
    public func load_html(view: WebView.ViewRef, html: String, base_url: String?) async throws {
        try await with(view) { try $0.loadHtml(html, baseUrl: base_url) }
    }
    public func navigate_history(view: WebView.ViewRef, nav: WebView.HistoryNav) async throws { try await with(view) { $0.navigate(nav) } }
    public func history(view: WebView.ViewRef) async throws -> WebView.WebHistory { try await with(view) { $0.history() } }
    public func eval(view: WebView.ViewRef, call: WebView.ScriptCall) async throws -> String {
        try await withAsync(view) { $0.eval(call, done: $1) }
    }
    public func post_message(view: WebView.ViewRef, message: WebView.OutgoingMessage) async throws {
        try await with(view) { try $0.postMessage(message) }
    }
    public func reply(view: WebView.ViewRef, reply: WebView.BridgeReply) async throws { try await with(view) { try $0.reply(reply) } }
    public func decide(view: WebView.ViewRef, decision: WebView.Decision) async throws { try await with(view) { try $0.decide(decision) } }
    public func scheme_head(view: WebView.ViewRef, head: WebView.SchemeResponseHead) async throws {
        try await with(view) { try self.schemeRouter.head($0, head) }
    }
    public func scheme_chunk(view: WebView.ViewRef, chunk: WebView.SchemeChunk) async throws {
        try await with(view) { try self.schemeRouter.chunk($0, chunk) }
    }
    public func scheme_request_body(view: WebView.ViewRef, request_id: UInt64) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { cont in
            DispatchQueue.main.async {
                do {
                    if let body = try self.schemeRouter.body(try self.view(view), request_id), !body.isEmpty {
                        cont.yield(body)
                    }
                    cont.finish()
                } catch {
                    cont.finish(throwing: error)
                }
            }
        }
    }
    public func cookies(view: WebView.ViewRef, filter: WebView.CookieFilter) async throws -> [WebView.WebCookie] {
        try await withAsync(view) { $0.cookies(filter, done: $1) }
    }
    public func set_cookie(view: WebView.ViewRef, cookie: WebView.WebCookie) async throws {
        try await withAsync(view) { $0.setCookie(cookie, done: $1) }
    }
    public func clear_data(view: WebView.ViewRef, request: WebView.ClearDataRequest) async throws {
        try await withAsync(view) { $0.clearData(request, done: $1) }
    }
    public func set_zoom(view: WebView.ViewRef, factor: Float) async throws {
        try await with(view) { $0.webView.pageZoom = CGFloat(factor) }
    }
    public func find(view: WebView.ViewRef, query: WebView.FindQuery) async throws -> WebView.FindResult {
        try await withAsync(view) { $0.find(query, done: $1) }
    }
    public func snapshot(view: WebView.ViewRef, options: WebView.SnapshotOptions) async throws -> WebView.WebSnapshot {
        try await withAsync(view) { $0.snapshot(options, done: $1) }
    }
    public func print(view: WebView.ViewRef) async throws {
        try await with(view) { v in
            let controller = UIPrintInteractionController.shared
            controller.printFormatter = v.webView.viewPrintFormatter()
            controller.present(animated: true)
        }
    }
    public func set_inspector(view: WebView.ViewRef, open: Bool) async throws {
        try await with(view) { v in
            guard self.config.allowInspector else { throw WebView.WebViewError.unsupported("inspector disabled") }
            if #available(iOS 16.4, *) {
                v.webView.isInspectable = open
            } else {
                throw WebView.WebViewError.unsupported("inspector needs iOS 16.4")
            }
        }
    }
    public func download_control(view: WebView.ViewRef, op: WebView.DownloadOp) async throws {
        try await with(view) { try $0.downloadControl(op) }
    }
    public func control_events(view: WebView.ViewRef) -> AsyncThrowingStream<WebView.ControlEvent, Error> {
        do {
            return try self.view(view).controlStream()
        } catch {
            return AsyncThrowingStream { $0.finish(throwing: error) }
        }
    }
    public func telemetry(view: WebView.ViewRef, kinds: [WebView.TelemetryKind]) -> AsyncThrowingStream<WebView.TelemetryEvent, Error> {
        do {
            return try self.view(view).telemetryStream(kinds)
        } catch {
            return AsyncThrowingStream { $0.finish(throwing: error) }
        }
    }
    #else
    private func none<T>() throws -> T { throw WebView.WebViewError.unsupported("WebKit") }
    public func set_frame(view: WebView.ViewRef, frame: WebView.ViewFrame) async throws { try none() as Void }
    public func focus(view: WebView.ViewRef) async throws { try none() as Void }
    public func load(view: WebView.ViewRef, request: WebView.LoadRequest) async throws { try none() as Void }
    public func load_html(view: WebView.ViewRef, html: String, base_url: String?) async throws { try none() as Void }
    public func navigate_history(view: WebView.ViewRef, nav: WebView.HistoryNav) async throws { try none() as Void }
    public func history(view: WebView.ViewRef) async throws -> WebView.WebHistory { try none() }
    public func eval(view: WebView.ViewRef, call: WebView.ScriptCall) async throws -> String { try none() }
    public func post_message(view: WebView.ViewRef, message: WebView.OutgoingMessage) async throws { try none() as Void }
    public func reply(view: WebView.ViewRef, reply: WebView.BridgeReply) async throws { try none() as Void }
    public func decide(view: WebView.ViewRef, decision: WebView.Decision) async throws { try none() as Void }
    public func scheme_head(view: WebView.ViewRef, head: WebView.SchemeResponseHead) async throws { try none() as Void }
    public func scheme_chunk(view: WebView.ViewRef, chunk: WebView.SchemeChunk) async throws { try none() as Void }
    public func scheme_request_body(view: WebView.ViewRef, request_id: UInt64) -> AsyncThrowingStream<Data, Error> {
        AsyncThrowingStream { $0.finish(throwing: WebView.WebViewError.unsupported("WebKit")) }
    }
    public func cookies(view: WebView.ViewRef, filter: WebView.CookieFilter) async throws -> [WebView.WebCookie] { try none() }
    public func set_cookie(view: WebView.ViewRef, cookie: WebView.WebCookie) async throws { try none() as Void }
    public func clear_data(view: WebView.ViewRef, request: WebView.ClearDataRequest) async throws { try none() as Void }
    public func set_zoom(view: WebView.ViewRef, factor: Float) async throws { try none() as Void }
    public func find(view: WebView.ViewRef, query: WebView.FindQuery) async throws -> WebView.FindResult { try none() }
    public func snapshot(view: WebView.ViewRef, options: WebView.SnapshotOptions) async throws -> WebView.WebSnapshot { try none() }
    public func print(view: WebView.ViewRef) async throws { try none() as Void }
    public func set_inspector(view: WebView.ViewRef, open: Bool) async throws { try none() as Void }
    public func download_control(view: WebView.ViewRef, op: WebView.DownloadOp) async throws { try none() as Void }
    public func control_events(view: WebView.ViewRef) -> AsyncThrowingStream<WebView.ControlEvent, Error> {
        AsyncThrowingStream { $0.finish(throwing: WebView.WebViewError.unsupported("WebKit")) }
    }
    public func telemetry(view: WebView.ViewRef, kinds: [WebView.TelemetryKind]) -> AsyncThrowingStream<WebView.TelemetryEvent, Error> {
        AsyncThrowingStream { $0.finish(throwing: WebView.WebViewError.unsupported("WebKit")) }
    }
    #endif
}
