import Foundation
import IstmoRuntime
#if canImport(GoogleSignIn)
import GoogleSignIn
import UIKit
#endif

public final class SignInFactoryImpl: SignInFactory {
    public init() {}

    public func create(config: SignInConfig) async throws -> SignInBackend {
        SignInBackendImpl(config: config)
    }
}

public final class SignInBackendImpl: SignInBackend, HandleReleaser {

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
            return try await restore(orThrow: .noCredentialAvailable)
        }
    }

    public func silent_sign_in() async throws -> SignInAccount? {
        do {
            return try await restore(orThrow: .noCredentialAvailable)
        } catch SignInError.noCredentialAvailable {
            return nil
        }
    }

    public func refresh(credential: NativeHandleId) async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        let user = credLock.withLock { credentials[credential] as? GIDGoogleUser }
        guard let user = user else {
            throw SignInError.reauthenticate
        }
        return try await withCheckedThrowingContinuation { cont in
            user.refreshTokensIfNeeded { refreshed, error in
                if let error = error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                guard let refreshed = refreshed else {
                    cont.resume(throwing: SignInError.reauthenticate)
                    return
                }

                self.credLock.withLock { self.credentials[credential] = refreshed }
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
        credLock.withLock { credentials.removeAll() }
        #endif
    }

    public func revoke() async throws {
        #if canImport(GoogleSignIn)
        let center = GIDSignIn.sharedInstance
        return try await withCheckedThrowingContinuation { cont in
            center.disconnect { error in
                if let error = error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                self.credLock.withLock { self.credentials.removeAll() }
                cont.resume()
            }
        }
        #endif
    }

    public func releaseNativeHandle(_ handleId: UInt64) {
        credLock.withLock { _ = credentials.removeValue(forKey: handleId) }
    }

    private func applyConfiguration() {
        #if canImport(GoogleSignIn)

        if GIDSignIn.sharedInstance.configuration == nil {
            let plistClientId = Bundle.main.object(forInfoDictionaryKey: "GIDClientID") as? String
            guard let plistClientId = plistClientId else {
                NSLog("SignInBackendImpl: GIDClientID missing from Info.plist — sign-in will fail")
                return
            }
            GIDSignIn.sharedInstance.configuration = GIDConfiguration(
                clientID: plistClientId,
                serverClientID: config.serverClientId,
                hostedDomain: config.hostedDomain,
                openIDRealm: nil,
            )
        }
        #endif
    }

    private func interactive() async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        let presenter = await Self.topViewController()
        guard let presenter = presenter else {
            throw SignInError.invalidConfiguration("no presenting UIViewController available")
        }
        return try await withCheckedThrowingContinuation { cont in
            GIDSignIn.sharedInstance.signIn(
                withPresenting: presenter,
                hint: nil,
                additionalScopes: config.scopes,
            ) { result, error in
                if let error = error {
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                guard let user = result?.user else {
                    cont.resume(throwing: SignInError.backend("empty GIDSignInResult"))
                    return
                }
                let id = IstmoRuntime.shared.allocHandleId(pluginId: SignInDispatcher.PLUGIN_ID)
                self.credLock.withLock { self.credentials[id] = user }
                cont.resume(returning: self.account(from: user, credentialId: id))
            }
        }
        #else
        throw SignInError.invalidConfiguration("GoogleSignIn SDK not linked")
        #endif
    }

    private func restore(orThrow err: SignInError) async throws -> SignInAccount {
        #if canImport(GoogleSignIn)
        return try await withCheckedThrowingContinuation { cont in
            GIDSignIn.sharedInstance.restorePreviousSignIn { user, error in
                if let error = error {

                    let nserror = error as NSError
                    if nserror.domain == kGIDSignInErrorDomain,
                       nserror.code == GIDSignInError.hasNoAuthInKeychain.rawValue {
                        cont.resume(throwing: err)
                        return
                    }
                    cont.resume(throwing: Self.mapSignInError(error))
                    return
                }
                guard let user = user else {
                    cont.resume(throwing: err)
                    return
                }
                let id = IstmoRuntime.shared.allocHandleId(pluginId: SignInDispatcher.PLUGIN_ID)
                self.credLock.withLock { self.credentials[id] = user }
                cont.resume(returning: self.account(from: user, credentialId: id))
            }
        }
        #else
        throw SignInError.invalidConfiguration("GoogleSignIn SDK not linked")
        #endif
    }

    #if canImport(GoogleSignIn)
    private func account(from user: GIDGoogleUser, credentialId: NativeHandleId) -> SignInAccount {
        let profile = user.profile
        let grantedScopes = user.grantedScopes ?? []
        return SignInAccount(
            id: user.userID ?? "",
            email: profile?.email,
            displayName: profile?.name,
            photoUrl: profile?.imageURL(withDimension: 256)?.absoluteString,
            idToken: user.idToken?.tokenString ?? "",
            grantedScopes: grantedScopes,
            credential: credentialId,
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
        let scenes = UIApplication.shared.connectedScenes
            .compactMap { $0 as? UIWindowScene }
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

private extension NSLock {

    @discardableResult
    func withLock<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}

