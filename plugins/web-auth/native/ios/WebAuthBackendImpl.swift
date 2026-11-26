import Foundation
import IstmoRuntime
#if canImport(AuthenticationServices)
import AuthenticationServices
#endif
#if canImport(UIKit)
import UIKit
#endif

/// Reference backend for `istmo.web_auth` on top of
/// `ASWebAuthenticationSession`: the system browser sheet, sharing
/// Safari's cookies unless the request is ephemeral.
///
/// Custom-scheme callbacks work on every supported iOS; `https`
/// callbacks need iOS 17.4 and an associated domain
/// (`webcredentials:` / `applinks:`) for the host. Loopback callbacks
/// are a desktop concept and are refused.
public final class WebAuthBackendImpl: NSObject, WebAuthBackend {
    private struct Prepared {
        let spec: WebAuth.CallbackSpec
        let redirect: URL
    }

    private let lock = NSLock()
    private var prepared: [UInt64: Prepared] = [:]
    private var busy = false

    public override init() {
        super.init()
    }

    public func capabilities() async throws -> WebAuth.WebAuthCapabilities {
        var https = false
        if #available(iOS 17.4, *) {
            https = true
        }
        return WebAuth.WebAuthCapabilities(
            backend: "aswebauthenticationsession",
            loopbackCallbacks: false,
            schemeCallbacks: true,
            httpsCallbacks: https,
            ephemeral: true,
            embedded: false
        )
    }

    public func prepare(callback: WebAuth.CallbackSpec) async throws -> WebAuth.PreparedRedirect {
        let uri: String
        switch callback {
        case .loopback:
            throw WebAuth.WebAuthError.unsupported("loopback callbacks on iOS; use a custom scheme or https")
        case .customScheme(let spec):
            let scheme = spec.scheme.lowercased()
            guard Self.isValidScheme(scheme), spec.path.isEmpty || spec.path.hasPrefix("/") else {
                throw WebAuth.WebAuthError.invalidArgument("callback scheme")
            }
            guard Self.appDeclares(scheme: scheme) else {
                throw WebAuth.WebAuthError.callbackNotRegistered
            }
            let path = spec.path.isEmpty ? "/" : spec.path
            uri = spec.host.isEmpty ? "\(scheme):\(path)" : "\(scheme)://\(spec.host.lowercased())\(path)"
        case .https(let spec):
            guard #available(iOS 17.4, *) else {
                throw WebAuth.WebAuthError.unsupported("https callbacks need iOS 17.4")
            }
            let path = spec.path.isEmpty ? "/" : spec.path
            uri = "https://\(spec.host.lowercased())\(path)"
        }
        guard let redirect = URL(string: uri) else {
            throw WebAuth.WebAuthError.invalidArgument("callback")
        }
        let session = store(Prepared(spec: callback, redirect: redirect))
        return WebAuth.PreparedRedirect(redirectUri: uri, session: session)
    }

    public func authenticate(request: WebAuth.AuthRequest) async throws -> WebAuth.AuthCallback {
        guard request.mode == .system else {
            throw WebAuth.WebAuthError.unsupported("embedded sign-in on iOS; ASWebAuthenticationSession is the supported path")
        }
        guard let authorize = URL(string: request.authorizeUrl), authorize.scheme == "https" else {
            throw WebAuth.WebAuthError.invalidArgument("authorize_url must be https")
        }
        let entry = try takeSession(request.session)
        defer { release() }
        let url = try await present(authorize: authorize, entry: entry, request: request)
        guard Self.queryValue(url, "state") == request.expectedState else {
            throw WebAuth.WebAuthError.stateMismatch
        }
        return WebAuth.AuthCallback(url: url.absoluteString)
    }

    // `NSLock` is unavailable from async contexts: every locked section is
    // a synchronous helper.
    private func store(_ entry: Prepared) -> UInt64 {
        lock.lock()
        defer { lock.unlock() }
        var session = Self.randomSession()
        while prepared[session] != nil {
            session = Self.randomSession()
        }
        prepared[session] = entry
        return session
    }

    private func takeSession(_ id: UInt64) throws -> Prepared {
        lock.lock()
        defer { lock.unlock() }
        if busy {
            throw WebAuth.WebAuthError.alreadyInProgress
        }
        guard let entry = prepared.removeValue(forKey: id) else {
            throw WebAuth.WebAuthError.unknownSession
        }
        busy = true
        return entry
    }

    private func release() {
        lock.lock()
        busy = false
        lock.unlock()
    }

    private func present(authorize: URL, entry: Prepared, request: WebAuth.AuthRequest) async throws -> URL {
        #if canImport(AuthenticationServices) && canImport(UIKit)
        let box = SessionBox()
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<URL, Error>) in
                DispatchQueue.main.async {
                    let completion: (URL?, Error?) -> Void = { url, error in
                        if let url = url {
                            cont.resume(returning: url)
                        } else if box.timedOut {
                            cont.resume(throwing: WebAuth.WebAuthError.timeout)
                        } else {
                            cont.resume(throwing: Self.map(error))
                        }
                    }
                    let session: ASWebAuthenticationSession
                    if #available(iOS 17.4, *) {
                        session = ASWebAuthenticationSession(
                            url: authorize, callback: Self.callback(for: entry), completionHandler: completion)
                    } else {
                        session = ASWebAuthenticationSession(
                            url: authorize, callbackURLScheme: entry.redirect.scheme, completionHandler: completion)
                    }
                    session.prefersEphemeralWebBrowserSession = request.ephemeral
                    session.presentationContextProvider = self
                    box.session = session
                    if !session.start() {
                        box.session = nil
                        cont.resume(throwing: WebAuth.WebAuthError.presentationUnavailable)
                        return
                    }
                    if request.timeoutMs > 0 {
                        DispatchQueue.main.asyncAfter(deadline: .now() + .milliseconds(Int(request.timeoutMs))) {
                            guard let live = box.session else { return }
                            box.timedOut = true
                            live.cancel()
                        }
                    }
                }
            }
        } onCancel: {
            DispatchQueue.main.async { box.session?.cancel() }
        }
        #else
        throw WebAuth.WebAuthError.noBrowser
        #endif
    }

    #if canImport(AuthenticationServices) && canImport(UIKit)
    @available(iOS 17.4, *)
    private static func callback(for entry: Prepared) -> ASWebAuthenticationSession.Callback {
        switch entry.spec {
        case .https(let spec):
            return .https(host: spec.host.lowercased(), path: spec.path.isEmpty ? "/" : spec.path)
        default:
            return .customScheme(entry.redirect.scheme ?? "")
        }
    }

    private static func map(_ error: Error?) -> WebAuth.WebAuthError {
        guard let error = error as? ASWebAuthenticationSessionError else {
            return .backend(error.map { "\($0)" } ?? "no callback")
        }
        switch error.code {
        case .canceledLogin:
            return .cancelled
        case .presentationContextNotProvided, .presentationContextInvalid:
            return .presentationUnavailable
        @unknown default:
            return .backend("\(error)")
        }
    }
    #endif

    private static func isValidScheme(_ scheme: String) -> Bool {
        guard let first = scheme.unicodeScalars.first, CharacterSet.lowercaseLetters.contains(first) else {
            return false
        }
        let allowed = CharacterSet(charactersIn: "abcdefghijklmnopqrstuvwxyz0123456789+-.")
        return scheme.unicodeScalars.allSatisfy { allowed.contains($0) }
            && !["http", "https", "file", "javascript", "data"].contains(scheme)
    }

    /// Whether `CFBundleURLTypes` (from `[[app.url_scheme]]`) lists it.
    private static func appDeclares(scheme: String) -> Bool {
        let types = Bundle.main.object(forInfoDictionaryKey: "CFBundleURLTypes") as? [[String: Any]] ?? []
        return types.contains { entry in
            (entry["CFBundleURLSchemes"] as? [String] ?? []).contains { $0.lowercased() == scheme }
        }
    }

    private static func queryValue(_ url: URL, _ name: String) -> String? {
        let components = URLComponents(url: url, resolvingAgainstBaseURL: false)
        if let value = components?.queryItems?.first(where: { $0.name == name })?.value {
            return value
        }
        var fragment = URLComponents()
        fragment.query = components?.fragment
        return fragment.queryItems?.first(where: { $0.name == name })?.value
    }

    private static func randomSession() -> UInt64 {
        var generator = SystemRandomNumberGenerator()
        var id: UInt64 = 0
        while id == 0 {
            id = generator.next()
        }
        return id
    }
}

/// Main-thread state of one presented session.
private final class SessionBox {
    #if canImport(AuthenticationServices) && canImport(UIKit)
    var session: ASWebAuthenticationSession?
    #endif
    var timedOut = false
}

#if canImport(AuthenticationServices) && canImport(UIKit)
extension WebAuthBackendImpl: ASWebAuthenticationPresentationContextProviding {
    public func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        let windows = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
            .flatMap { $0.windows }
        return windows.first(where: { $0.isKeyWindow }) ?? windows.first ?? ASPresentationAnchor()
    }
}
#endif
