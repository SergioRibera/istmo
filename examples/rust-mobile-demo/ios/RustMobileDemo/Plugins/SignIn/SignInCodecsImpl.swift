// Codec impl for the `SignIn` plugin's `Named` types.
//
// Wire schema mirrors the Rust `#[message]` types verbatim:
//
// * `SignInMode` — u32 varint discriminant.
// * `SignInConfig` — String, Vec<String>, Option<String>, Option<String>, Bool.
// * `SignInAccount` — String, Option<String> × 3, String, Vec<String>,
//   NativeHandleId.
// * `SignInError` — u32 varint discriminant + per-variant payload
//   (String for InvalidConfiguration / Network / Backend).
// * `NativeHandleId` — single u64 varint.

import Foundation
import IstmoRuntime

public final class SignInCodecsImpl: SignInCodecs {

    public init() {}

    // MARK: - SignInMode

    public func readSignInMode(_ c: inout Bincode.Cursor) throws -> SignInMode {
        let disc = try Bincode.readVarintU32(&c)
        guard let value = SignInMode(rawValue: disc) else {
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
        return value
    }

    public func writeSignInMode(_ out: inout Data, _ value: SignInMode) {
        Bincode.writeVarintU32(&out, value.rawValue)
    }

    // MARK: - SignInConfig

    public func readSignInConfig(_ c: inout Bincode.Cursor) throws -> SignInConfig {
        let server = try Bincode.readString(&c)
        let scopes = try Bincode.readVec(&c) { try Bincode.readString(&$0) }
        let hosted = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        let nonce = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        let autoSelect = try Bincode.readBool(&c)
        return SignInConfig(
            serverClientId: server,
            scopes: scopes,
            hostedDomain: hosted,
            nonce: nonce,
            autoSelect: autoSelect,
        )
    }

    public func writeSignInConfig(_ out: inout Data, _ value: SignInConfig) {
        Bincode.writeString(&out, value.serverClientId)
        Bincode.writeVec(&out, value.scopes) { Bincode.writeString(&$0, $1) }
        Bincode.writeOption(&out, value.hostedDomain) { Bincode.writeString(&$0, $1) }
        Bincode.writeOption(&out, value.nonce) { Bincode.writeString(&$0, $1) }
        Bincode.writeBool(&out, value.autoSelect)
    }

    // MARK: - SignInAccount

    public func readSignInAccount(_ c: inout Bincode.Cursor) throws -> SignInAccount {
        let id = try Bincode.readString(&c)
        let email = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        let display = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        let photo = try Bincode.readOption(&c) { try Bincode.readString(&$0) }
        let idToken = try Bincode.readString(&c)
        let scopes = try Bincode.readVec(&c) { try Bincode.readString(&$0) }
        let credential = try readNativeHandleId(&c)
        return SignInAccount(
            id: id,
            email: email,
            displayName: display,
            photoUrl: photo,
            idToken: idToken,
            grantedScopes: scopes,
            credential: credential,
        )
    }

    public func writeSignInAccount(_ out: inout Data, _ value: SignInAccount) {
        Bincode.writeString(&out, value.id)
        Bincode.writeOption(&out, value.email) { Bincode.writeString(&$0, $1) }
        Bincode.writeOption(&out, value.displayName) { Bincode.writeString(&$0, $1) }
        Bincode.writeOption(&out, value.photoUrl) { Bincode.writeString(&$0, $1) }
        Bincode.writeString(&out, value.idToken)
        Bincode.writeVec(&out, value.grantedScopes) { Bincode.writeString(&$0, $1) }
        writeNativeHandleId(&out, value.credential)
    }

    // MARK: - SignInError

    public func readSignInError(_ c: inout Bincode.Cursor) throws -> SignInError {
        let disc = try Bincode.readVarintU32(&c)
        switch disc {
        case 0: return .userCancelled
        case 1: return .noCredentialAvailable
        case 2: return .reauthenticate
        case 3: return .invalidConfiguration(try Bincode.readString(&c))
        case 4: return .network(try Bincode.readString(&c))
        case 5: return .backend(try Bincode.readString(&c))
        default:
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
    }

    public func writeSignInError(_ out: inout Data, _ value: SignInError) {
        switch value {
        case .userCancelled:
            Bincode.writeVarintU32(&out, 0)
        case .noCredentialAvailable:
            Bincode.writeVarintU32(&out, 1)
        case .reauthenticate:
            Bincode.writeVarintU32(&out, 2)
        case .invalidConfiguration(let s):
            Bincode.writeVarintU32(&out, 3)
            Bincode.writeString(&out, s)
        case .network(let s):
            Bincode.writeVarintU32(&out, 4)
            Bincode.writeString(&out, s)
        case .backend(let s):
            Bincode.writeVarintU32(&out, 5)
            Bincode.writeString(&out, s)
        }
    }

    // MARK: - NativeHandleId

    public func readNativeHandleId(_ c: inout Bincode.Cursor) throws -> NativeHandleId {
        try Bincode.readVarintU64(&c)
    }

    public func writeNativeHandleId(_ out: inout Data, _ value: NativeHandleId) {
        Bincode.writeVarintU64(&out, value)
    }
}
