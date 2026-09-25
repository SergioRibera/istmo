import Foundation
import IstmoRuntime
#if canImport(LocalAuthentication)
import LocalAuthentication
#endif

/// Reference `LocalAuthentication` backend for `istmo.biometric`.
///
/// Register it once the runtime is up:
///
/// ```swift
/// IstmoRuntime.shared.registerHandler(
///     BiometricDispatcher.PLUGIN_ID,
///     BiometricDispatcher(backend: BiometricBackendImpl(), codecs: BiometricCodecsImpl())
/// )
/// ```
///
/// Face ID requires `NSFaceIDUsageDescription` in the app's `Info.plist`;
/// `istmo-build` writes it from the plugin's `[plugin.info_plist]`
/// (override the text with `[app.info_plist]`).
///
/// Secrets are generic-password Keychain items (service
/// `dev.istmo.biometric`) guarded by a `SecAccessControl`:
/// `.biometryCurrentSet` for biometric-only policies — enrolling a new
/// face or finger makes the item unreadable — and `.userPresence` when
/// the passcode may stand in.
public final class BiometricBackendImpl: BiometricBackend {

    public init() {}

    // ------------------------------------------------------- Biometric impl

    public func availability(policy: AuthPolicy) async throws -> Availability {
        #if canImport(LocalAuthentication)
        let context = LAContext()
        var biometricError: NSError?
        let biometrics = context.canEvaluatePolicy(
            .deviceOwnerAuthenticationWithBiometrics, error: &biometricError)
        // `biometryType` is only set after a `canEvaluatePolicy` call.
        let kinds = Self.kinds(of: context)
        var credentialError: NSError?
        let credential = context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &credentialError)

        let (evaluated, error) = policy == .biometricOrDeviceCredential
            ? (credential, credentialError)
            : (biometrics, biometricError)
        return Availability(
            status: evaluated ? .available : Self.status(for: error),
            kinds: kinds,
            deviceCredentialAvailable: credential
        )
        #else
        return Availability(status: .unsupported, kinds: [], deviceCredentialAvailable: false)
        #endif
    }

    public func authenticate(prompt: AuthPrompt) async throws -> AuthMethod {
        guard !prompt.reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw BiometricError.invalidPrompt("`reason` must not be empty")
        }
        #if canImport(LocalAuthentication)
        let context = LAContext()
        if let label = prompt.cancelLabel {
            context.localizedCancelTitle = label
        }
        if let label = prompt.fallbackLabel {
            // An empty string hides the fallback button.
            context.localizedFallbackTitle = label
        }
        let policy = Self.laPolicy(prompt.policy)
        let box = ContextBox(context)

        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Void, Error>) in
                box.context.evaluatePolicy(policy, localizedReason: prompt.reason) { success, error in
                    if success {
                        cont.resume()
                    } else {
                        cont.resume(throwing: Self.error(for: error))
                    }
                }
            }
        } onCancel: {
            // Dismisses the sheet; the reply then fires with `appCancel`.
            box.context.invalidate()
        }

        switch prompt.policy {
        case .biometricStrong, .biometricWeak:
            return .biometric
        case .biometricOrDeviceCredential:
            // LocalAuthentication does not say whether Face ID / Touch ID
            // or the passcode satisfied `.deviceOwnerAuthentication`.
            return .unspecified
        }
        #else
        throw BiometricError.notAvailable(.unsupported)
        #endif
    }

    public func store_secret(alias: String, secret: Data, prompt: AuthPrompt) async throws {
        try Self.checkAlias(alias)
        #if canImport(LocalAuthentication)
        let flags: SecAccessControlCreateFlags = prompt.policy == .biometricOrDeviceCredential
            ? .userPresence
            : .biometryCurrentSet
        var error: Unmanaged<CFError>?
        guard let access = SecAccessControlCreateWithFlags(
            nil, kSecAttrAccessibleWhenPasscodeSetThisDeviceOnly, flags, &error
        ) else {
            let reason = error?.takeRetainedValue().localizedDescription ?? "unknown"
            throw BiometricError.backend("SecAccessControlCreateWithFlags: \(reason)")
        }
        try Self.deleteItem(alias)
        var attributes = Self.itemQuery(alias)
        attributes[kSecValueData as String] = secret
        attributes[kSecAttrAccessControl as String] = access
        try Self.check(SecItemAdd(attributes as CFDictionary, nil))
        #else
        throw BiometricError.unsupportedOperation("the Keychain is unavailable on this platform")
        #endif
    }

    public func read_secret(alias: String, prompt: AuthPrompt) async throws -> Data {
        try Self.checkAlias(alias)
        guard !prompt.reason.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else {
            throw BiometricError.invalidPrompt("`reason` must not be empty")
        }
        #if canImport(LocalAuthentication)
        let context = LAContext()
        context.localizedReason = prompt.reason
        if let label = prompt.cancelLabel {
            context.localizedCancelTitle = label
        }
        if let label = prompt.fallbackLabel {
            context.localizedFallbackTitle = label
        }
        var query = Self.itemQuery(alias)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        query[kSecUseAuthenticationContext as String] = context
        let box = ContextBox(context)
        let lookup = QueryBox(query)

        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { (cont: CheckedContinuation<Data, Error>) in
                // `SecItemCopyMatching` blocks while the Face ID sheet is up.
                DispatchQueue.global(qos: .userInitiated).async {
                    var result: CFTypeRef?
                    let status = SecItemCopyMatching(lookup.query as CFDictionary, &result)
                    do {
                        try Self.check(status)
                        guard let data = result as? Data else {
                            throw BiometricError.backend("Keychain returned no data")
                        }
                        cont.resume(returning: data)
                    } catch {
                        cont.resume(throwing: error)
                    }
                }
            }
        } onCancel: {
            box.context.invalidate()
        }
        #else
        throw BiometricError.unsupportedOperation("the Keychain is unavailable on this platform")
        #endif
    }

    public func delete_secret(alias: String) async throws {
        try Self.checkAlias(alias)
        #if canImport(LocalAuthentication)
        try Self.deleteItem(alias)
        #endif
    }

    public func has_secret(alias: String) async throws -> Bool {
        try Self.checkAlias(alias)
        #if canImport(LocalAuthentication)
        let context = LAContext()
        context.interactionNotAllowed = true
        var query = Self.itemQuery(alias)
        query[kSecReturnAttributes as String] = true
        query[kSecUseAuthenticationContext as String] = context
        switch SecItemCopyMatching(query as CFDictionary, nil) {
        case errSecSuccess, errSecInteractionNotAllowed:
            // The item exists; reading it would need authentication.
            return true
        case errSecItemNotFound:
            return false
        case let status:
            try Self.check(status)
            return false
        }
        #else
        return false
        #endif
    }

    // ------------------------------------------------------- helpers

    private static let keychainService = "dev.istmo.biometric"

    private static func checkAlias(_ alias: String) throws {
        let allowed = CharacterSet(charactersIn:
            "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789._-")
        guard (1...64).contains(alias.utf8.count),
              alias.unicodeScalars.allSatisfy(allowed.contains) else {
            throw BiometricError.invalidAlias(alias)
        }
    }

    #if canImport(LocalAuthentication)
    /// Lets the cancellation handler reach the context; `LAContext` is
    /// documented as safe to invalidate from any thread.
    private final class ContextBox: @unchecked Sendable {
        let context: LAContext
        init(_ context: LAContext) { self.context = context }
    }

    /// Hands the immutable query to the Keychain worker queue.
    private final class QueryBox: @unchecked Sendable {
        let query: [String: Any]
        init(_ query: [String: Any]) { self.query = query }
    }

    private static func itemQuery(_ alias: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: keychainService,
            kSecAttrAccount as String: alias,
        ]
    }

    private static func deleteItem(_ alias: String) throws {
        let status = SecItemDelete(itemQuery(alias) as CFDictionary)
        guard status != errSecItemNotFound else { return }
        try check(status)
    }

    private static func check(_ status: OSStatus) throws {
        switch status {
        case errSecSuccess:
            return
        case errSecItemNotFound:
            throw BiometricError.secretNotFound
        case errSecUserCanceled:
            throw BiometricError.userCancelled
        case errSecAuthFailed:
            throw BiometricError.authFailed
        case errSecInteractionNotAllowed:
            throw BiometricError.systemCancelled
        case errSecParam where !LAContext().canEvaluatePolicy(.deviceOwnerAuthentication, error: nil):
            // Access-controlled items need a device passcode.
            throw BiometricError.notAvailable(.noneEnrolled)
        default:
            let message = SecCopyErrorMessageString(status, nil) as String? ?? "OSStatus \(status)"
            throw BiometricError.backend("Keychain: \(message)")
        }
    }

    private static func laPolicy(_ policy: AuthPolicy) -> LAPolicy {
        switch policy {
        case .biometricStrong, .biometricWeak:
            return .deviceOwnerAuthenticationWithBiometrics
        case .biometricOrDeviceCredential:
            return .deviceOwnerAuthentication
        }
    }

    private static func kinds(of context: LAContext) -> [BiometricKind] {
        switch context.biometryType {
        case .touchID:
            return [.fingerprint]
        case .faceID:
            return [.face]
        case .none:
            return []
        default:
            // `.opticID` (iOS 17 / visionOS) and future sensors.
            if #available(iOS 17.0, *), context.biometryType == .opticID {
                return [.iris]
            }
            return [.other]
        }
    }

    private static func code(of error: Error?) -> LAError.Code? {
        guard let error = error as NSError?, error.domain == LAErrorDomain else { return nil }
        return LAError.Code(rawValue: error.code)
    }

    private static func status(for error: Error?) -> BiometricStatus {
        switch code(of: error) {
        case .passcodeNotSet, .biometryNotEnrolled:
            return .noneEnrolled
        case .biometryNotAvailable:
            return .noHardware
        case .biometryLockout:
            return .lockedOut
        default:
            return .hardwareUnavailable
        }
    }

    private static func error(for error: Error?) -> BiometricError {
        switch code(of: error) {
        case .authenticationFailed:
            return .authFailed
        case .userCancel:
            return .userCancelled
        case .userFallback:
            return .userFallback
        case .systemCancel, .appCancel:
            return .systemCancelled
        case .biometryLockout:
            // Unlocks only after the device passcode.
            return .lockedOutPermanent
        case .passcodeNotSet, .biometryNotEnrolled, .biometryNotAvailable:
            return .notAvailable(status(for: error))
        default:
            return .backend(error.map { "\($0)" } ?? "LocalAuthentication failed without an error")
        }
    }
    #endif
}
