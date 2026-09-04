// Swift mirrors of the Rust `#[message]` types in
// `istmo-plugins/src/google_sign_in.rs`.
//
// `NativeHandleId` is not defined here — it is a common wire type shared
// across plugins (see `IstmoRuntime.allocHandleId`). We alias it once so
// codegen dispatchers can name it directly.

import Foundation

/// Wire type alias so the dispatcher's `readNativeHandleId` signature
/// lines up with the codegen output. Rust `NativeHandleId` is a `u64`
/// newtype; on the wire it's a single u64 varint.
public typealias NativeHandleId = UInt64

public enum SignInMode: UInt32 {
    case interactive = 0
    case silentOnly = 1
}

public struct SignInConfig {
    public let serverClientId: String
    public let scopes: [String]
    public let hostedDomain: String?
    public let nonce: String?
    public let autoSelect: Bool

    public init(
        serverClientId: String,
        scopes: [String],
        hostedDomain: String?,
        nonce: String?,
        autoSelect: Bool
    ) {
        self.serverClientId = serverClientId
        self.scopes = scopes
        self.hostedDomain = hostedDomain
        self.nonce = nonce
        self.autoSelect = autoSelect
    }
}

public struct SignInAccount {
    public let id: String
    public let email: String?
    public let displayName: String?
    public let photoUrl: String?
    public let idToken: String
    public let grantedScopes: [String]
    public let credential: NativeHandleId

    public init(
        id: String,
        email: String?,
        displayName: String?,
        photoUrl: String?,
        idToken: String,
        grantedScopes: [String],
        credential: NativeHandleId
    ) {
        self.id = id
        self.email = email
        self.displayName = displayName
        self.photoUrl = photoUrl
        self.idToken = idToken
        self.grantedScopes = grantedScopes
        self.credential = credential
    }
}

/// Domain error. Backends throw a case; the codecs encode it via
/// `SignInCodecsImpl.writeSignInError` for the wire.
public enum SignInError: Error {
    case userCancelled
    case noCredentialAvailable
    case reauthenticate
    case invalidConfiguration(String)
    case network(String)
    case backend(String)
}
