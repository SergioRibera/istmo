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

    // ------------------------------------------------------- helpers

    #if canImport(LocalAuthentication)
    /// Lets the cancellation handler reach the context; `LAContext` is
    /// documented as safe to invalidate from any thread.
    private final class ContextBox: @unchecked Sendable {
        let context: LAContext
        init(_ context: LAContext) { self.context = context }
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
