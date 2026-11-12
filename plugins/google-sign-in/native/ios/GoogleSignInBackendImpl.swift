import Foundation
import IstmoRuntime
#if canImport(GoogleSignIn)
import GoogleSignIn
import UIKit
#endif

/// One `GoogleSignInBackendImpl` per `SignInClient` instance.
public final class GoogleSignInFactoryImpl: SignInFactory {
    public init() {}

    public func create(config: SignInConfig) async throws -> SignInBackend {
        GoogleSignInBackendImpl(config: config)
    }
}

/// Reference `GoogleSignIn-iOS` (`GIDSignIn`) backend for
/// `istmo.google_sign_in`.
///
/// The iOS OAuth client id is read from `GIDClientID` in `Info.plist`
/// (the SDK's own convention); `SignInConfig.serverClientId` becomes the
/// id-token audience. The app must also register the reversed client id
/// as a URL scheme and forward `application(_:open:options:)` to
/// `GIDSignIn.sharedInstance.handle(_:)`.
public final class GoogleSignInBackendImpl: SignInBackend, HandleReleaser {

    private let config: SignInConfig
    private let credLock = NSLock()
    private var credentials: [NativeHandleId: Any] = [:]

    public init(config: SignInConfig) {
        self.config = config
        applyConfiguration()
    }

    public func sign_in(mode: SignInMode) async throws -> SignInAccount {
        switch mode {
        case .interactive:
            return try await interactive()
        case .silentOnly:
            return try await restore()
        }
    }

    public func silent_sign_in() async throws -> SignInAccount? {
        do {
            return try await restore()
        } catch SignInError.noCredentialAvailable {
            return nil
        }
    }

    public func refresh(credential: NativeHandleId) async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        let user = locked { credentials[credential] as? GIDGoogleUser }
        guard let user else {
            throw SignInError.reauthenticate
        }
        return try await withCheckedThrowingContinuation { cont in
            user.refreshTokensIfNeeded { refreshed, error in
                if let error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                guard let refreshed else {
                    cont.resume(throwing: SignInError.reauthenticate)
                    return
                }
                self.locked { self.credentials[credential] = refreshed }
                cont.resume(returning: self.account(from: refreshed, credentialId: credential))
            }
        }
        #else
        throw SignInError.invalidConfiguration("GoogleSignIn SDK not linked")
        #endif
    }

    public func sign_out() async throws {
        #if canImport(GoogleSignIn)
        GIDSignIn.sharedInstance.signOut()
        forgetAll()
        #endif
    }

    public func revoke() async throws {
        #if canImport(GoogleSignIn)
        try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
            GIDSignIn.sharedInstance.disconnect { error in
                if let error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                self.forgetAll()
                cont.resume()
            }
        }
        #endif
    }

    public func releaseNativeHandle(_ handleId: UInt64) {
        let removed = locked { credentials.removeValue(forKey: handleId) }
        if removed != nil {
            IstmoRuntime.shared.forgetHandle(handleId)
        }
    }

    private func locked<T>(_ body: () -> T) -> T {
        credLock.lock()
        defer { credLock.unlock() }
        return body()
    }

    private func forgetAll() {
        let ids = locked { () -> [NativeHandleId] in
            let ids = Array(credentials.keys)
            credentials.removeAll()
            return ids
        }
        for id in ids {
            IstmoRuntime.shared.forgetHandle(id)
        }
    }

    private func applyConfiguration() {
        #if canImport(GoogleSignIn)
        guard GIDSignIn.sharedInstance.configuration == nil else { return }
        guard let clientId = Bundle.main.object(forInfoDictionaryKey: "GIDClientID") as? String else {
            NSLog("GoogleSignInBackendImpl: GIDClientID missing from Info.plist; sign-in will fail")
            return
        }
        GIDSignIn.sharedInstance.configuration = GIDConfiguration(
            clientID: clientId,
            serverClientID: config.serverClientId,
            hostedDomain: config.hostedDomain,
            openIDRealm: nil
        )
        #endif
    }

    private func interactive() async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        guard GIDSignIn.sharedInstance.configuration != nil else {
            throw SignInError.invalidConfiguration("GIDClientID missing from Info.plist")
        }
        guard let presenter = await Self.topViewController() else {
            throw SignInError.invalidConfiguration("no presenting UIViewController available")
        }
        return try await withCheckedThrowingContinuation { cont in
            DispatchQueue.main.async {
                GIDSignIn.sharedInstance.signIn(
                    withPresenting: presenter,
                    hint: nil,
                    additionalScopes: self.config.scopes,
                    nonce: self.config.nonce
                ) { result, error in
                    if let error {
                        cont.resume(throwing: Self.mapSignInError(error))
                        return
                    }
                    guard let user = result?.user else {
                        cont.resume(throwing: SignInError.backend("empty GIDSignInResult"))
                        return
                    }
                    cont.resume(returning: self.adopt(user))
                }
            }
        }
        #else
        throw SignInError.invalidConfiguration("GoogleSignIn SDK not linked")
        #endif
    }

    private func restore() async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        return try await withCheckedThrowingContinuation { cont in
            GIDSignIn.sharedInstance.restorePreviousSignIn { user, error in
                if let error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                guard let user else {
                    cont.resume(throwing: SignInError.noCredentialAvailable)
                    return
                }
                cont.resume(returning: self.adopt(user))
            }
        }
        #else
        throw SignInError.invalidConfiguration("GoogleSignIn SDK not linked")
        #endif
    }

    #if canImport(GoogleSignIn)
    private func adopt(_ user: GIDGoogleUser) -> SignInAccount {
        let id = IstmoRuntime.shared.allocHandleId(pluginId: SignInDispatcher.PLUGIN_ID)
        locked { credentials[id] = user }
        return account(from: user, credentialId: id)
    }

    private func account(from user: GIDGoogleUser, credentialId: NativeHandleId) -> SignInAccount {
        let profile = user.profile
        return SignInAccount(
            id: user.userID ?? "",
            email: profile?.email,
            displayName: profile?.name,
            photoUrl: profile?.imageURL(withDimension: 256)?.absoluteString,
            idToken: user.idToken?.tokenString ?? "",
            grantedScopes: user.grantedScopes ?? [],
            credential: credentialId
        )
    }

    private static func mapSignInError(_ error: Error) -> SignInError {
        let nserror = error as NSError
        if nserror.domain == kGIDSignInErrorDomain {
            switch GIDSignInError.Code(rawValue: nserror.code) {
            case .canceled: return .userCancelled
            case .hasNoAuthInKeychain: return .noCredentialAvailable
            case .keychain: return .backend("keychain: \(nserror.localizedDescription)")
            default: break
            }
        }
        if nserror.domain == NSURLErrorDomain {
            return .network(nserror.localizedDescription)
        }
        return .backend(nserror.localizedDescription)
    }

    @MainActor
    private static func topViewController() -> UIViewController? {
        let scenes = UIApplication.shared.connectedScenes.compactMap { $0 as? UIWindowScene }
        for scene in scenes {
            for window in scene.windows where window.isKeyWindow {
                var vc = window.rootViewController
                while let presented = vc?.presentedViewController {
                    vc = presented
                }
                return vc
            }
        }
        return nil
    }
    #endif
}
