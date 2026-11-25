#if canImport(WebKit) && canImport(UIKit)
import CryptoKit
import Foundation
import IstmoRuntime
import UIKit
import WebKit

/// One `WKWebView` and its state. Main thread only.
final class ManagedWebView: NSObject {
    weak var backend: WebViewBackendImpl?
    let id: UInt64
    let tokenHi: UInt64
    let tokenLo: UInt64
    let spec: WebViewSpec
    let webView: WKWebView
    private let rules: CompiledRules

    // Presentation.
    private var controller: UIViewController?
    private var pendingFrame: ViewFrame?
    private var frameScheduled = false

    // Lanes.
    private var controlContinuation: AsyncThrowingStream<ControlEvent, Error>.Continuation?
    private var controlBuffer: [ControlEvent] = []
    private var controlTaken = false
    private var telemetrySubs: [UUID: (Set<TelemetryKind>, AsyncThrowingStream<TelemetryEvent, Error>.Continuation)] = [:]
    private var telemetryDropped: [UUID: UInt32] = [:]
    private var observations: [NSKeyValueObservation] = []

    // Navigation state.
    private var navigationId: UInt64 = 0
    private(set) var documentId: UInt64 = 0
    private(set) var committedOrigin: String?
    private var lastProgress = -1

    // Pending requests, keyed by single-use random ids.
    private enum Ask {
        case navigation((WKNavigationActionPolicy) -> Void, String)
        case auth((URLSession.AuthChallengeDisposition, URLCredential?) -> Void)
        case permission(Any)
        case alert(() -> Void)
        case confirm((Bool) -> Void)
        case prompt((String?) -> Void)
        case download(Any, (URL?) -> Void)
    }

    private var asks: [UInt64: Ask] = [:]
    private var replies: [UInt64: (Any?, String?) -> Void] = [:]
    var schemeTasks: [UInt64: WKURLSchemeTask] = [:]
    private var downloads: [UInt64: Any] = [:]
    private(set) var closed = false

    init(backend: WebViewBackendImpl, id: UInt64, tokenHi: UInt64, tokenLo: UInt64,
         spec: WebViewSpec, inherited: WKWebViewConfiguration?) throws {
        self.backend = backend
        self.id = id
        self.tokenHi = tokenHi
        self.tokenLo = tokenLo
        self.spec = spec
        rules = try CompiledRules(spec.rules)
        let configuration = try inherited ?? ManagedWebView.configuration(spec, backend: backend)
        webView = WKWebView(frame: .zero, configuration: configuration)
        super.init()
        configure()
        attach()
    }

    // MARK: setup

    private static func configuration(_ spec: WebViewSpec, backend: WebViewBackendImpl) throws -> WKWebViewConfiguration {
        let c = WKWebViewConfiguration()
        switch spec.profile {
        case .shared:
            c.websiteDataStore = .default()
        case .ephemeral:
            c.websiteDataStore = .nonPersistent()
        case .persistent(let profile):
            guard #available(iOS 17.0, *) else { throw WebViewError.profileUnsupported }
            c.websiteDataStore = WKWebsiteDataStore(forIdentifier: profileUUID(profile.name))
        }
        let s = spec.settings
        c.defaultWebpagePreferences.allowsContentJavaScript = s.javascript
        c.mediaTypesRequiringUserActionForPlayback = s.mediaAutoplay ? [] : .all
        c.allowsInlineMediaPlayback = true
        if let suffix = backend.config.userAgentSuffix { c.applicationNameForUserAgent = suffix }
        for scheme in backend.config.schemes {
            c.setURLSchemeHandler(backend.schemeRouter, forURLScheme: scheme.name)
        }
        let content = c.userContentController
        for script in spec.userScripts {
            let world: WKContentWorld = script.world == .isolated ? .defaultClient : .page
            let source = script.allowedOrigins.isEmpty ? script.source : originGuard(script.allowedOrigins, script.source)
            content.addUserScript(WKUserScript(
                source: source,
                injectionTime: script.injectAt == .documentStart ? .atDocumentStart : .atDocumentEnd,
                forMainFrameOnly: script.mainFrameOnly,
                in: world))
        }
        return c
    }

    /// Same UUID for the same profile name across launches.
    private static func profileUUID(_ name: String) -> UUID {
        var bytes = Array(SHA256.hash(data: Data(Array("istmo.webview.\(name)".utf8))).prefix(16))
        bytes[6] = (bytes[6] & 0x0f) | 0x50
        bytes[8] = (bytes[8] & 0x3f) | 0x80
        return UUID(uuid: (bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
                           bytes[8], bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15]))
    }

    private static func originGuard(_ origins: [String], _ source: String) -> String {
        let list = origins.map { "\"\(jsEscape($0))\"" }.joined(separator: ",")
        return "if ([\(list)].includes(location.origin)) {\n\(source)\n}"
    }

    private func configure() {
        let s = spec.settings
        webView.navigationDelegate = self
        webView.uiDelegate = self
        webView.allowsBackForwardNavigationGestures = true
        webView.isOpaque = !s.transparent
        webView.backgroundColor = s.transparent ? .clear : UIColor(
            red: CGFloat(s.background.r) / 255, green: CGFloat(s.background.g) / 255,
            blue: CGFloat(s.background.b) / 255, alpha: CGFloat(s.background.a) / 255)
        if let ua = s.userAgent { webView.customUserAgent = ua }
        if !s.zoomGestures { webView.scrollView.pinchGestureRecognizer?.isEnabled = false }
        installBridge()
        observations = [
            webView.observe(\.estimatedProgress) { [weak self] web, _ in self?.progress(web.estimatedProgress) },
            webView.observe(\.title) { [weak self] web, _ in self?.telemetry(.title, .title(web.title ?? "")) },
            webView.observe(\.url) { [weak self] web, _ in self?.telemetry(.url, .url(web.url?.absoluteString ?? "")) },
            webView.observe(\.canGoBack) { [weak self] web, _ in
                self?.telemetry(.history, .history(HistoryState(canGoBack: web.canGoBack, canGoForward: web.canGoForward)))
            },
        ]
    }

    private func installBridge() {
        guard let bridge = spec.bridge, !bridge.allowedOrigins.isEmpty else { return }
        let content = webView.configuration.userContentController
        content.addScriptMessageHandler(self, contentWorld: .page, name: Self.nativeName)
        let origins = bridge.allowedOrigins.map { "\"\(Self.jsEscape($0))\"" }.joined(separator: ",")
        content.addUserScript(WKUserScript(
            source: Self.shim(origins: origins, subframes: bridge.allowSubframes),
            injectionTime: .atDocumentStart,
            forMainFrameOnly: !bridge.allowSubframes,
            in: .page))
    }

    static let nativeName = "__istmoNative"

    private static func shim(origins: String, subframes: Bool) -> String {
        """
        (() => {
          const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.\(nativeName);
          if (!handler || window.istmo || ![\(origins)].includes(location.origin)\(subframes ? "" : " || window !== window.top")) return;
          const listeners = new Set();
          const toB64 = (d) => {
            const bytes = typeof d === "string" ? new TextEncoder().encode(d) : d instanceof Uint8Array ? d : new Uint8Array(d);
            let bin = "";
            for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode.apply(null, bytes.subarray(i, i + 0x8000));
            return btoa(bin);
          };
          const fromB64 = (b) => Uint8Array.from(atob(b), (c) => c.charCodeAt(0));
          Object.defineProperty(window, "__istmoDeliver", {
            value: (channel, b) => listeners.forEach((f) => { try { f({ channel, data: fromB64(b) }); } catch (e) { console.error(e); } }),
            configurable: false, writable: false, enumerable: false,
          });
          Object.defineProperty(window, "istmo", {
            value: Object.freeze({
              postMessage(channel, data) { handler.postMessage({ t: "m", c: String(channel), b: toB64(data ?? "") }); },
              invoke(channel, data) {
                return handler.postMessage({ t: "i", c: String(channel), b: toB64(data ?? "") }).then(fromB64);
              },
              addEventListener(type, f) { if (type === "message") listeners.add(f); },
              removeEventListener(type, f) { if (type === "message") listeners.delete(f); },
            }),
            configurable: false, writable: false,
          });
        })();
        """
    }

    static func jsEscape(_ s: String) -> String {
        var out = ""
        for scalar in s.unicodeScalars {
            switch scalar {
            case "\\": out += "\\\\"
            case "\"": out += "\\\""
            case "\n": out += "\\n"
            case "\r": out += "\\r"
            case "<": out += "\\u003c"
            default:
                if scalar.value < 0x20 || scalar.value == 0x2028 || scalar.value == 0x2029 {
                    out += String(format: "\\u%04x", scalar.value)
                } else {
                    out.unicodeScalars.append(scalar)
                }
            }
        }
        return out
    }

    // MARK: presentation

    private func topViewController() -> UIViewController? {
        let windows = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }.flatMap { $0.windows }
        var top = (windows.first(where: { $0.isKeyWindow }) ?? windows.first)?.rootViewController
        while let presented = top?.presentedViewController { top = presented }
        return top
    }

    private func attach() {
        switch spec.target {
        case .embedded:
            guard let root = topViewController()?.view else { return }
            webView.isHidden = true
            root.addSubview(webView)
        case .window(let target):
            let vc = UIViewController()
            vc.view = webView
            vc.title = target.title
            vc.modalPresentationStyle = target.modal ? .fullScreen : .pageSheet
            vc.presentationController?.delegate = self
            controller = vc
            topViewController()?.present(vc, animated: true)
        }
    }

    /// Latest frame wins; applied once per run-loop turn.
    func setFrame(_ frame: ViewFrame) {
        pendingFrame = frame
        guard !frameScheduled else { return }
        frameScheduled = true
        DispatchQueue.main.async { [weak self] in
            guard let self = self else { return }
            self.frameScheduled = false
            if let f = self.pendingFrame { self.apply(f) }
            self.pendingFrame = nil
        }
    }

    private func apply(_ frame: ViewFrame) {
        let scale = webView.window?.screen.scale ?? UIScreen.main.scale
        webView.isHidden = !frame.visible || frame.width == 0 || frame.height == 0
        webView.frame = CGRect(x: CGFloat(frame.x) / scale, y: CGFloat(frame.y) / scale,
                               width: CGFloat(frame.width) / scale, height: CGFloat(frame.height) / scale)
        webView.superview?.bringSubviewToFront(webView)
    }

    // MARK: lanes

    func emit(_ event: ControlEvent) {
        guard !closed else { return }
        if let cont = controlContinuation {
            cont.yield(event)
        } else if controlBuffer.count < 1024 {
            controlBuffer.append(event)
        }
    }

    /// Called from the runtime's task: hops to main for the bookkeeping.
    func controlStream() -> AsyncThrowingStream<ControlEvent, Error> {
        AsyncThrowingStream(bufferingPolicy: .unbounded) { cont in
            DispatchQueue.main.async {
                guard !self.controlTaken else {
                    cont.finish(throwing: WebViewError.invalidArgument("control events already have a subscriber"))
                    return
                }
                self.controlTaken = true
                self.controlBuffer.forEach { cont.yield($0) }
                self.controlBuffer.removeAll()
                self.controlContinuation = cont
                cont.onTermination = { _ in
                    DispatchQueue.main.async {
                        self.controlContinuation = nil
                        self.controlTaken = false
                    }
                }
            }
        }
    }

    func telemetryStream(_ kinds: [TelemetryKind]) -> AsyncThrowingStream<TelemetryEvent, Error> {
        let missing = kinds.filter { !spec.telemetry.contains($0) }
        if !missing.isEmpty {
            return AsyncThrowingStream { $0.finish(throwing: WebViewError.invalidArgument("telemetry kinds not in the view spec: \(missing)")) }
        }
        let wanted = Set(kinds)
        let key = UUID()
        return AsyncThrowingStream(bufferingPolicy: .bufferingNewest(64)) { cont in
            DispatchQueue.main.async {
                self.telemetrySubs[key] = (wanted, cont)
                cont.onTermination = { _ in
                    DispatchQueue.main.async {
                        self.telemetrySubs.removeValue(forKey: key)
                        self.telemetryDropped.removeValue(forKey: key)
                    }
                }
            }
        }
    }

    private func wants(_ kind: TelemetryKind) -> Bool { telemetrySubs.values.contains { $0.0.contains(kind) } }

    private func telemetry(_ kind: TelemetryKind, _ event: TelemetryEvent) {
        for (key, (kinds, cont)) in telemetrySubs where kinds.contains(kind) {
            if let dropped = telemetryDropped.removeValue(forKey: key) { cont.yield(.dropped(dropped)) }
            if case .dropped = cont.yield(event) {
                telemetryDropped[key, default: 0] += 1
            }
        }
    }

    private func progress(_ value: Double) {
        let percent = Int(value * 100)
        guard percent != lastProgress, wants(.progress) else { return }
        lastProgress = percent
        telemetry(.progress, .progress(Float(value)))
    }

    // MARK: navigation

    private func nextNavigation() -> UInt64 {
        navigationId += 1
        return navigationId
    }

    func requestId() -> UInt64 {
        var generator = SystemRandomNumberGenerator()
        while true {
            let id: UInt64 = generator.next()
            if id != 0 && asks[id] == nil && replies[id] == nil && schemeTasks[id] == nil { return id }
        }
    }

    func load(_ request: LoadRequest) {
        let info = NavInfo(navigationId: nextNavigation(), url: request.url, isRedirect: false, userGesture: false)
        switch rules.evaluate(request.url, fileAccess: spec.settings.fileAccess) {
        case .allow, .ask:
            guard let url = URL(string: request.url) else {
                emit(.navigationFailed(NavFailure(navigationId: info.navigationId, url: request.url, kind: .badUrl, description: "invalid URL")))
                return
            }
            var urlRequest = URLRequest(url: url)
            request.headers.forEach { urlRequest.setValue($0.value, forHTTPHeaderField: $0.name) }
            if let lang = spec.settings.acceptLanguage { urlRequest.setValue(lang, forHTTPHeaderField: "Accept-Language") }
            if let body = request.postBody {
                urlRequest.httpMethod = "POST"
                urlRequest.httpBody = body
            }
            // `.ask` is decided by `decidePolicyFor`, which sees this load.
            webView.load(urlRequest)
        case .deny:
            emit(.navigationFailed(NavFailure(navigationId: info.navigationId, url: request.url, kind: .cancelled, description: "blocked by navigation rules")))
        case .intercept:
            emit(.intercepted(InterceptedNav(navigationId: info.navigationId, url: request.url)))
        case .external:
            openExternally(request.url)
        }
    }

    func loadHtml(_ html: String, baseUrl: String?) throws {
        if let bridge = spec.bridge, let base = baseUrl {
            guard let origin = CompiledRules.origin(of: base), bridge.allowedOrigins.contains(origin) else {
                throw WebViewError.invalidArgument("base_url origin must be in the bridge's allowed_origins")
            }
        }
        webView.loadHTMLString(html, baseURL: baseUrl.flatMap(URL.init(string:)))
    }

    private func openExternally(_ url: String) {
        guard let target = URL(string: url) else { return }
        UIApplication.shared.open(target, options: [:], completionHandler: nil)
    }

    func navigate(_ nav: HistoryNav) {
        switch nav {
        case .back: webView.goBack()
        case .forward: webView.goForward()
        case .reload: webView.reload()
        case .reloadBypassCache: webView.reloadFromOrigin()
        case .stop: webView.stopLoading()
        case .offset(let n):
            let list = n < 0 ? webView.backForwardList.backList.reversed() : webView.backForwardList.forwardList
            let index = Int(n.magnitude) - 1
            if index >= 0 && index < list.count { webView.go(to: Array(list)[index]) }
        }
    }

    func history() -> WebHistory {
        let list = webView.backForwardList
        let entries = list.backList + (list.currentItem.map { [$0] } ?? []) + list.forwardList
        return WebHistory(
            entries: entries.map { HistoryEntry(url: $0.url.absoluteString, title: $0.title ?? "") },
            current: UInt32(list.backList.count))
    }

    // MARK: scripts and bridge

    func eval(_ call: ScriptCall, done: @escaping (Result<String, Error>) -> Void) {
        guard committedOrigin == call.expectedOrigin else { return done(.failure(WebViewError.originMismatch)) }
        let world: WKContentWorld = call.world == .isolated ? .defaultClient : .page
        // `callAsyncJavaScript` awaits promises; JSON keeps the answer typed.
        webView.callAsyncJavaScript(
            "return JSON.stringify(await (async () => { return eval(__istmoSource); })()) ?? \"null\";",
            arguments: ["__istmoSource": call.source], in: nil, in: world
        ) { result in
            switch result {
            case .success(let value): done(.success(value as? String ?? "null"))
            case .failure(let error): done(.failure(WebViewError.backend(error.localizedDescription)))
            }
        }
    }

    func postMessage(_ message: OutgoingMessage) throws {
        guard committedOrigin == message.expectedOrigin else { throw WebViewError.originMismatch }
        let script = "window.__istmoDeliver && window.__istmoDeliver(\"\(Self.jsEscape(message.channel))\", \"\(message.data.base64EncodedString())\");"
        webView.evaluateJavaScript(script, in: nil, in: .page, completionHandler: nil)
    }

    func reply(_ reply: BridgeReply) throws {
        guard let handler = replies.removeValue(forKey: reply.replyId) else { throw WebViewError.unknownRequest }
        switch reply.result {
        case .ok(let data): handler(data.base64EncodedString(), nil)
        case .err(let message): handler(nil, message)
        }
    }

    // MARK: decisions

    private func ask(_ ask: Ask, _ kind: DecisionKind, timeoutMs: UInt32?) {
        let id = requestId()
        asks[id] = ask
        emit(.decisionRequested(DecisionRequest(requestId: id, kind: kind)))
        if let timeout = timeoutMs, timeout > 0 {
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(timeout))) { [weak self] in
                if let pending = self?.asks.removeValue(forKey: id) { self?.resolve(pending, .deny) }
            }
        }
    }

    func decide(_ decision: Decision) throws {
        guard let ask = asks.removeValue(forKey: decision.requestId) else { throw WebViewError.unknownRequest }
        resolve(ask, decision.response)
    }

    private func resolve(_ ask: Ask, _ response: DecisionResponse) {
        let allowed: Bool
        if case .deny = response { allowed = false } else { allowed = true }
        switch ask {
        case .navigation(let handler, _):
            handler(allowed ? .allow : .cancel)
        case .auth(let handler):
            if case .credentials(let c) = response {
                handler(.useCredential, URLCredential(user: c.user, password: c.password, persistence: .forSession))
            } else {
                handler(.cancelAuthenticationChallenge, nil)
            }
        case .permission(let handler):
            if #available(iOS 15.0, *), let decide = handler as? (WKPermissionDecision) -> Void {
                decide(allowed ? .grant : .deny)
            }
        case .alert(let handler):
            handler()
        case .confirm(let handler):
            handler(allowed)
        case .prompt(let handler):
            if case .dialogAccept(let text) = response { handler(text ?? "") } else { handler(nil) }
        case .download(_, let handler):
            if case .downloadTo(let path) = response { handler(URL(fileURLWithPath: path)) } else { handler(nil) }
        }
    }

    private func failPending() {
        let pending = asks.values
        asks.removeAll()
        pending.forEach { resolve($0, .deny) }
        replies.values.forEach { $0(nil, "document closed") }
        replies.removeAll()
        schemeTasks.values.forEach { $0.didFailWithError(URLError(.cancelled)) }
        schemeTasks.removeAll()
    }

    func downloadControl(_ op: DownloadOp) throws {
        guard op.action == .cancel else { throw WebViewError.unsupported("pausing downloads") }
        guard let download = downloads.removeValue(forKey: op.downloadId) else { throw WebViewError.unknownRequest }
        if #available(iOS 14.5, *), let d = download as? WKDownload { d.cancel(nil) }
    }

    // MARK: profile data

    func cookies(_ filter: CookieFilter, done: @escaping (Result<[WebCookie], Error>) -> Void) {
        let host = filter.url.flatMap { URL(string: $0)?.host }?.lowercased()
        webView.configuration.websiteDataStore.httpCookieStore.getAllCookies { all in
            let matching = all.filter { cookie in
                if let name = filter.name, cookie.name != name { return false }
                guard let host = host else { return true }
                let domain = cookie.domain.lowercased().trimmingPrefixDot()
                return host == domain || host.hasSuffix("." + domain)
            }
            done(.success(matching.map(ManagedWebView.webCookie)))
        }
    }

    private static func webCookie(_ c: HTTPCookie) -> WebCookie {
        let sameSite: SameSite
        switch c.sameSitePolicy {
        case .some(.sameSiteStrict): sameSite = .strict
        case .some(.sameSiteLax): sameSite = .lax
        default: sameSite = .none
        }
        return WebCookie(
            name: c.name, value: c.value, domain: c.domain, path: c.path,
            expiresUnixMs: c.expiresDate.map { Int64($0.timeIntervalSince1970 * 1000) },
            secure: c.isSecure, httpOnly: c.isHTTPOnly, sameSite: sameSite)
    }

    func setCookie(_ cookie: WebCookie, done: @escaping (Result<Void, Error>) -> Void) {
        var properties: [HTTPCookiePropertyKey: Any] = [
            .name: cookie.name, .value: cookie.value, .domain: cookie.domain,
            .path: cookie.path.isEmpty ? "/" : cookie.path,
        ]
        if let expires = cookie.expiresUnixMs { properties[.expires] = Date(timeIntervalSince1970: Double(expires) / 1000) }
        if cookie.secure { properties[.secure] = "TRUE" }
        if cookie.httpOnly { properties[HTTPCookiePropertyKey("HttpOnly")] = "TRUE" }
        switch cookie.sameSite {
        case .strict: properties[.sameSitePolicy] = HTTPCookieStringPolicy.sameSiteStrict
        case .lax: properties[.sameSitePolicy] = HTTPCookieStringPolicy.sameSiteLax
        case .none: break
        }
        guard let http = HTTPCookie(properties: properties) else {
            return done(.failure(WebViewError.invalidArgument("cookie")))
        }
        webView.configuration.websiteDataStore.httpCookieStore.setCookie(http) { done(.success(())) }
    }

    func clearData(_ request: ClearDataRequest, done: @escaping (Result<Void, Error>) -> Void) {
        var types = Set<String>()
        for kind in request.kinds {
            switch kind {
            case .cookies: types.insert(WKWebsiteDataTypeCookies)
            case .cache: types.formUnion([WKWebsiteDataTypeDiskCache, WKWebsiteDataTypeMemoryCache])
            case .localStorage: types.formUnion([WKWebsiteDataTypeLocalStorage, WKWebsiteDataTypeSessionStorage])
            case .indexedDb: types.insert(WKWebsiteDataTypeIndexedDBDatabases)
            case .serviceWorkers: types.insert(WKWebsiteDataTypeServiceWorkerRegistrations)
            case .all: types = WKWebsiteDataStore.allWebsiteDataTypes()
            }
        }
        let since = request.sinceUnixMs.map { Date(timeIntervalSince1970: Double($0) / 1000) } ?? .distantPast
        webView.configuration.websiteDataStore.removeData(ofTypes: types, modifiedSince: since) { done(.success(())) }
    }

    // MARK: misc

    func find(_ query: FindQuery, done: @escaping (Result<FindResult, Error>) -> Void) {
        guard #available(iOS 16.0, *) else { return done(.failure(WebViewError.unsupported("find needs iOS 16"))) }
        let config = WKFindConfiguration()
        config.backwards = query.backwards
        config.caseSensitive = query.caseSensitive
        config.wraps = query.wrap
        webView.find(query.text, configuration: config) { result in
            // WebKit reports a match, not a count.
            done(.success(FindResult(matches: result.matchFound ? 1 : 0, active: result.matchFound ? 1 : 0)))
        }
    }

    func snapshot(_ options: SnapshotOptions, done: @escaping (Result<WebSnapshot, Error>) -> Void) {
        let config = WKSnapshotConfiguration()
        if options.maxWidth > 0 { config.snapshotWidth = NSNumber(value: Double(options.maxWidth) / Double(UIScreen.main.scale)) }
        webView.takeSnapshot(with: config) { image, error in
            guard let image = image else {
                return done(.failure(WebViewError.backend(error?.localizedDescription ?? "snapshot failed")))
            }
            let data: Data?
            switch options.format {
            case .png: data = image.pngData()
            case .jpeg(let q): data = image.jpegData(compressionQuality: CGFloat(min(q, 100)) / 100)
            }
            guard let bytes = data, let cg = image.cgImage else { return done(.failure(WebViewError.backend("encode"))) }
            done(.success(WebSnapshot(width: UInt32(cg.width), height: UInt32(cg.height), format: options.format, bytes: bytes)))
        }
    }

    func close() {
        guard !closed else { return }
        failPending()
        closed = true
        observations.forEach { $0.invalidate() }
        observations.removeAll()
        webView.stopLoading()
        webView.configuration.userContentController.removeAllScriptMessageHandlers()
        webView.navigationDelegate = nil
        webView.uiDelegate = nil
        if let vc = controller {
            vc.presentationController?.delegate = nil
            vc.dismiss(animated: true)
        }
        webView.removeFromSuperview()
        controlContinuation?.finish()
        telemetrySubs.values.forEach { $0.1.finish() }
        telemetrySubs.removeAll()
    }
}

// MARK: - WKScriptMessageHandlerWithReply

extension ManagedWebView: WKScriptMessageHandlerWithReply {
    func userContentController(_ controller: WKUserContentController, didReceive message: WKScriptMessage,
                               replyHandler: @escaping (Any?, String?) -> Void) {
        guard let bridge = spec.bridge else { return replyHandler(nil, "no bridge") }
        let origin = message.frameInfo.securityOrigin
        let defaultPort = CompiledRules.defaultPorts[origin.protocol]
        let originText = "\(origin.protocol)://\(origin.host)" + (origin.port == 0 || origin.port == defaultPort ? "" : ":\(origin.port)")
        let isMain = message.frameInfo.isMainFrame
        guard bridge.allowedOrigins.contains(originText), isMain || bridge.allowSubframes,
              let body = message.body as? [String: Any], let type = body["t"] as? String,
              let channel = body["c"] as? String, bridge.channels.contains(channel),
              let b64 = body["b"] as? String, let data = Data(base64Encoded: b64)
        else { return replyHandler(nil, "not allowed") }
        var replyId: UInt64?
        if type == "i" {
            let id = requestId()
            replies[id] = replyHandler
            replyId = id
        } else {
            replyHandler(nil, nil)
        }
        emit(.message(BridgeMessage(channel: channel, origin: originText, isMainFrame: isMain,
                                    documentId: documentId, replyId: replyId, data: data)))
    }
}

// MARK: - WKNavigationDelegate

extension ManagedWebView: WKNavigationDelegate {
    func webView(_ webView: WKWebView, decidePolicyFor action: WKNavigationAction,
                 decisionHandler: @escaping (WKNavigationActionPolicy) -> Void) {
        guard action.targetFrame?.isMainFrame ?? true, let url = action.request.url?.absoluteString else {
            return decisionHandler(.allow)
        }
        let info = NavInfo(navigationId: nextNavigation(), url: url, isRedirect: false,
                           userGesture: action.navigationType == .linkActivated)
        switch rules.evaluate(url, fileAccess: spec.settings.fileAccess) {
        case .allow:
            decisionHandler(.allow)
        case .deny:
            decisionHandler(.cancel)
            emit(.navigationFailed(NavFailure(navigationId: info.navigationId, url: url, kind: .cancelled, description: "blocked by navigation rules")))
        case .intercept:
            decisionHandler(.cancel)
            emit(.intercepted(InterceptedNav(navigationId: info.navigationId, url: url)))
        case .external:
            decisionHandler(.cancel)
            openExternally(url)
        case .ask:
            // WebKit lets the navigation wait for the app.
            ask(.navigation(decisionHandler, url), .navigation(info), timeoutMs: spec.timeouts.navigationMs)
        }
    }

    func webView(_ webView: WKWebView, decidePolicyFor response: WKNavigationResponse,
                 decisionHandler: @escaping (WKNavigationResponsePolicy) -> Void) {
        if !response.canShowMIMEType, #available(iOS 14.5, *) {
            decisionHandler(.download)
        } else {
            decisionHandler(.allow)
        }
    }

    func webView(_ webView: WKWebView, didStartProvisionalNavigation navigation: WKNavigation!) {
        emit(.navigationStarted(NavInfo(navigationId: navigationId, url: webView.url?.absoluteString ?? "", isRedirect: false, userGesture: false)))
    }

    func webView(_ webView: WKWebView, didCommit navigation: WKNavigation!) {
        documentId += 1
        let url = webView.url?.absoluteString ?? ""
        committedOrigin = CompiledRules.origin(of: url) ?? webView.url.flatMap { u in u.scheme.map { s in "\(s)://\(u.host ?? "")" } }
        // Replies owed to the previous document can no longer arrive.
        replies.values.forEach { $0(nil, "document replaced") }
        replies.removeAll()
        emit(.navigationCommitted(NavInfo(navigationId: navigationId, url: url, isRedirect: false, userGesture: false)))
    }

    func webView(_ webView: WKWebView, didFinish navigation: WKNavigation!) {
        emit(.navigationFinished(NavInfo(navigationId: navigationId, url: webView.url?.absoluteString ?? "", isRedirect: false, userGesture: false)))
    }

    func webView(_ webView: WKWebView, didFail navigation: WKNavigation!, withError error: Error) {
        failed(error)
    }

    func webView(_ webView: WKWebView, didFailProvisionalNavigation navigation: WKNavigation!, withError error: Error) {
        failed(error)
    }

    private func failed(_ error: Error) {
        let ns = error as NSError
        let kind: NavErrorKind
        switch ns.code {
        case NSURLErrorCannotFindHost, NSURLErrorDNSLookupFailed: kind = .dns
        case NSURLErrorTimedOut: kind = .timeout
        case NSURLErrorSecureConnectionFailed, NSURLErrorServerCertificateUntrusted, NSURLErrorServerCertificateHasBadDate,
             NSURLErrorServerCertificateNotYetValid, NSURLErrorServerCertificateHasUnknownRoot: kind = .tls
        case NSURLErrorCancelled: kind = .cancelled
        case NSURLErrorBadURL, NSURLErrorUnsupportedURL: kind = .badUrl
        case NSURLErrorNotConnectedToInternet, NSURLErrorNetworkConnectionLost, NSURLErrorCannotConnectToHost: kind = .network
        default: kind = .other
        }
        let url = (ns.userInfo[NSURLErrorFailingURLStringErrorKey] as? String) ?? ""
        emit(.navigationFailed(NavFailure(navigationId: navigationId, url: url, kind: kind, description: ns.localizedDescription)))
    }

    func webView(_ webView: WKWebView, didReceive challenge: URLAuthenticationChallenge,
                 completionHandler: @escaping (URLSession.AuthChallengeDisposition, URLCredential?) -> Void) {
        let method = challenge.protectionSpace.authenticationMethod
        guard method == NSURLAuthenticationMethodHTTPBasic || method == NSURLAuthenticationMethodHTTPDigest else {
            // Server trust stays with the system: certificate errors fail closed.
            return completionHandler(.performDefaultHandling, nil)
        }
        let space = challenge.protectionSpace
        ask(.auth(completionHandler), .httpAuth(HttpAuthAsk(host: space.host, realm: space.realm ?? "", isProxy: space.isProxy())), timeoutMs: nil)
    }

    func webViewWebContentProcessDidTerminate(_ webView: WKWebView) {
        failPending()
        emit(.processTerminated(TerminationInfo(reason: .crashed)))
    }

    @available(iOS 14.5, *)
    func webView(_ webView: WKWebView, navigationResponse: WKNavigationResponse, didBecome download: WKDownload) {
        download.delegate = self
    }

    @available(iOS 14.5, *)
    func webView(_ webView: WKWebView, navigationAction: WKNavigationAction, didBecome download: WKDownload) {
        download.delegate = self
    }
}

// MARK: - WKDownloadDelegate

@available(iOS 14.5, *)
extension ManagedWebView: WKDownloadDelegate {
    func download(_ download: WKDownload, decideDestinationUsing response: URLResponse, suggestedFilename: String,
                  completionHandler: @escaping (URL?) -> Void) {
        let id = requestId()
        downloads[id] = download
        let name = suggestedFilename.replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "\\", with: "_")
        let length = response.expectedContentLength >= 0 ? UInt64(response.expectedContentLength) : nil
        asks[id] = .download(download, completionHandler)
        emit(.decisionRequested(DecisionRequest(requestId: id, kind: .download(DownloadAsk(
            downloadId: id, url: response.url?.absoluteString ?? "", suggestedName: name,
            mimeType: response.mimeType ?? "", contentLength: length)))))
    }

    func downloadDidFinish(_ download: WKDownload) {
        guard let id = downloads.first(where: { ($0.value as AnyObject) === download })?.key else { return }
        downloads.removeValue(forKey: id)
        telemetry(.downloads, .downloadFinished(DownloadDone(downloadId: id, path: nil, error: nil)))
    }

    func download(_ download: WKDownload, didFailWithError error: Error, resumeData: Data?) {
        guard let id = downloads.first(where: { ($0.value as AnyObject) === download })?.key else { return }
        downloads.removeValue(forKey: id)
        telemetry(.downloads, .downloadFinished(DownloadDone(downloadId: id, path: nil, error: error.localizedDescription)))
    }
}

// MARK: - WKUIDelegate

extension ManagedWebView: WKUIDelegate {
    func webView(_ webView: WKWebView, createWebViewWith configuration: WKWebViewConfiguration,
                 for action: WKNavigationAction, windowFeatures: WKWindowFeatures) -> WKWebView? {
        // WebKit wants the new view synchronously: the rules decide.
        let url = action.request.url?.absoluteString ?? ""
        guard rules.evaluate(url, fileAccess: spec.settings.fileAccess) == .allow, let backend = backend else { return nil }
        let childSpec = WebViewSpec(
            target: .window(WindowTarget(title: "", width: 0, height: 0, parentWindowId: nil, modal: false, resizable: true)),
            profile: spec.profile, settings: spec.settings, rules: spec.rules, userScripts: spec.userScripts,
            bridge: spec.bridge, timeouts: spec.timeouts, telemetry: spec.telemetry)
        guard let child = try? backend.create(spec: childSpec, configuration: configuration) else { return nil }
        emit(.newView(NewViewInfo(instance: child.instance, url: url)))
        return child.webView
    }

    func webViewDidClose(_ webView: WKWebView) {
        emit(.closeRequested)
    }

    func webView(_ webView: WKWebView, runJavaScriptAlertPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping () -> Void) {
        ask(.alert(completionHandler), .dialog(DialogAsk(kind: .alert, message: message, defaultText: "", origin: origin(frame))), timeoutMs: nil)
    }

    func webView(_ webView: WKWebView, runJavaScriptConfirmPanelWithMessage message: String,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (Bool) -> Void) {
        ask(.confirm(completionHandler), .dialog(DialogAsk(kind: .confirm, message: message, defaultText: "", origin: origin(frame))), timeoutMs: nil)
    }

    func webView(_ webView: WKWebView, runJavaScriptTextInputPanelWithPrompt prompt: String, defaultText: String?,
                 initiatedByFrame frame: WKFrameInfo, completionHandler: @escaping (String?) -> Void) {
        ask(.prompt(completionHandler), .dialog(DialogAsk(kind: .prompt, message: prompt, defaultText: defaultText ?? "", origin: origin(frame))), timeoutMs: nil)
    }

    @available(iOS 15.0, *)
    func webView(_ webView: WKWebView, requestMediaCapturePermissionFor origin: WKSecurityOrigin,
                 initiatedByFrame frame: WKFrameInfo, type: WKMediaCaptureType,
                 decisionHandler: @escaping (WKPermissionDecision) -> Void) {
        let kinds: [WebPermission]
        switch type {
        case .camera: kinds = [.camera]
        case .microphone: kinds = [.microphone]
        case .cameraAndMicrophone: kinds = [.camera, .microphone]
        @unknown default: kinds = [.other]
        }
        let frameOrigin = "\(origin.protocol)://\(origin.host)"
        ask(.permission(decisionHandler), .permission(PermissionAsk(permissions: kinds, frameOrigin: frameOrigin, topOrigin: committedOrigin ?? frameOrigin)), timeoutMs: nil)
    }

    private func origin(_ frame: WKFrameInfo) -> String {
        "\(frame.securityOrigin.protocol)://\(frame.securityOrigin.host)"
    }
}

// MARK: - dismissal of windowed views

extension ManagedWebView: UIAdaptivePresentationControllerDelegate {
    func presentationControllerDidDismiss(_ presentationController: UIPresentationController) {
        emit(.closeRequested)
    }
}

extension ManagedWebView {
    var instance: WebViewInstance { WebViewInstance(view: id, tokenHi: tokenHi, tokenLo: tokenLo) }
}

private extension String {
    func trimmingPrefixDot() -> String { hasPrefix(".") ? String(dropFirst()) : self }
}

/// Serves the app's custom schemes for every view of a backend.
final class SchemeRouter: NSObject, WKURLSchemeHandler {
    private weak var backend: WebViewBackendImpl?
    private var owners: [ObjectIdentifier: (ManagedWebView, UInt64)] = [:]

    init(backend: WebViewBackendImpl) {
        self.backend = backend
    }

    private func owner(of webView: WKWebView) -> ManagedWebView? {
        guard let backend = backend else { return nil }
        // Views are few; a linear scan avoids a second registry.
        return backend.allViews().first { $0.webView === webView }
    }

    func webView(_ webView: WKWebView, start task: WKURLSchemeTask) {
        guard let view = owner(of: webView), let url = task.request.url, let scheme = url.scheme,
              let config = backend?.config.schemes.first(where: { $0.name == scheme })
        else {
            return task.didFailWithError(URLError(.unsupportedURL))
        }
        switch config.source {
        case .assets(let source):
            serveAsset(task, url: url, dir: source.dir)
        case .dynamic:
            let id = view.requestId()
            view.schemeTasks[id] = task
            owners[ObjectIdentifier(task)] = (view, id)
            let headers = (task.request.allHTTPHeaderFields ?? [:]).map { HttpHeader(name: $0.key, value: $0.value) }
            view.emit(.schemeRequest(SchemeRequestInfo(
                requestId: id, scheme: scheme, method: task.request.httpMethod ?? "GET", url: url.absoluteString,
                headers: headers, hasBody: task.request.httpBody != nil, initiatorOrigin: view.committedOrigin)))
            let timeout = Int(view.spec.timeouts.schemeHeadMs)
            DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(max(timeout, 1))) { [weak view] in
                guard let view = view, let pending = view.schemeTasks[id], pending === task, !self.started.contains(id) else { return }
                view.schemeTasks.removeValue(forKey: id)
                self.respond(task, url: url, status: 504, mime: "text/plain", body: Data())
            }
        }
    }

    func webView(_ webView: WKWebView, stop task: WKURLSchemeTask) {
        if let (view, id) = owners.removeValue(forKey: ObjectIdentifier(task)) {
            view.schemeTasks.removeValue(forKey: id)
            started.remove(id)
        }
    }

    private var started = Set<UInt64>()

    func head(_ view: ManagedWebView, _ head: SchemeResponseHead) throws {
        guard let task = view.schemeTasks[head.requestId], !started.contains(head.requestId), let url = task.request.url else {
            throw WebViewError.unknownRequest
        }
        started.insert(head.requestId)
        var headers = ["Content-Type": head.mimeType, "X-Content-Type-Options": "nosniff"]
        for header in head.headers where !(header.name.lowercased() == "access-control-allow-origin" && header.value == "*") {
            headers[header.name] = header.value
        }
        if let length = head.contentLength { headers["Content-Length"] = String(length) }
        guard let response = HTTPURLResponse(url: url, statusCode: Int(head.status), httpVersion: "HTTP/1.1", headerFields: headers) else {
            throw WebViewError.invalidArgument("response head")
        }
        task.didReceive(response)
    }

    func chunk(_ view: ManagedWebView, _ chunk: SchemeChunk) throws {
        guard let task = view.schemeTasks[chunk.requestId], started.contains(chunk.requestId) else {
            throw WebViewError.unknownRequest
        }
        switch chunk.body {
        case .data(let data):
            task.didReceive(data)
        case .end:
            task.didFinish()
            finish(view, chunk.requestId, task)
        case .abort:
            task.didFailWithError(URLError(.cancelled))
            finish(view, chunk.requestId, task)
        }
    }

    func body(_ view: ManagedWebView, _ id: UInt64) throws -> Data? {
        guard let task = view.schemeTasks[id] else { throw WebViewError.unknownRequest }
        return task.request.httpBody
    }

    private func finish(_ view: ManagedWebView, _ id: UInt64, _ task: WKURLSchemeTask) {
        view.schemeTasks.removeValue(forKey: id)
        started.remove(id)
        owners.removeValue(forKey: ObjectIdentifier(task))
    }

    private func serveAsset(_ task: WKURLSchemeTask, url: URL, dir: String) {
        let segments = url.path.split(separator: "/", omittingEmptySubsequences: false).dropFirst().map(String.init)
        guard !segments.contains(where: { $0.isEmpty || $0 == "." || $0 == ".." || $0.contains("\\") || $0.contains("\u{0}") }) || url.path == "/"
        else { return respond(task, url: url, status: 400, mime: "text/plain", body: Data()) }
        let relative = (url.path == "/" || url.path.isEmpty) ? ["index.html"] : segments
        let resource = Bundle.main.resourceURL ?? URL(fileURLWithPath: "/")
        let roots = [resource.appendingPathComponent("Assets"), resource]
        let ext = (relative.last.map { ($0 as NSString).pathExtension.lowercased() }) ?? ""
        guard let mime = SchemeRouter.mime[ext] else { return respond(task, url: url, status: 415, mime: "text/plain", body: Data()) }
        for root in roots {
            var file = root
            for component in dir.split(separator: "/") + relative.map({ Substring($0) }) {
                file.appendPathComponent(String(component))
            }
            // Resolve links and stay inside the root.
            let resolved = file.resolvingSymlinksInPath().standardizedFileURL.path
            guard resolved.hasPrefix(root.resolvingSymlinksInPath().standardizedFileURL.path + "/") else { continue }
            if let data = FileManager.default.contents(atPath: resolved) {
                return respond(task, url: url, status: 200, mime: mime, body: data)
            }
        }
        respond(task, url: url, status: 404, mime: "text/plain", body: Data())
    }

    private func respond(_ task: WKURLSchemeTask, url: URL, status: Int, mime: String, body: Data) {
        let headers = ["Content-Type": mime, "Content-Length": String(body.count), "X-Content-Type-Options": "nosniff", "Cache-Control": "no-cache"]
        guard let response = HTTPURLResponse(url: url, statusCode: status, httpVersion: "HTTP/1.1", headerFields: headers) else { return }
        task.didReceive(response)
        task.didReceive(body)
        task.didFinish()
    }

    private static let mime: [String: String] = [
        "html": "text/html", "htm": "text/html", "css": "text/css", "js": "text/javascript", "mjs": "text/javascript",
        "json": "application/json", "wasm": "application/wasm", "svg": "image/svg+xml", "png": "image/png",
        "jpg": "image/jpeg", "jpeg": "image/jpeg", "gif": "image/gif", "webp": "image/webp", "avif": "image/avif",
        "ico": "image/x-icon", "woff": "font/woff", "woff2": "font/woff2", "ttf": "font/ttf", "otf": "font/otf",
        "txt": "text/plain", "xml": "application/xml", "mp4": "video/mp4", "webm": "video/webm", "mp3": "audio/mpeg",
        "ogg": "audio/ogg", "wav": "audio/wav", "pdf": "application/pdf", "map": "application/json",
    ]
}
#endif
