import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif

/// Reference backend for `istmo.opener` on top of `UIApplication.open`.
///
/// Only URLs whose scheme is in `allowedSchemes` are opened, mirroring
/// the Rust-side `SchemePolicy`; anything else fails with
/// `OpenError.schemeNotAllowed`.
public final class OpenerBackendImpl: OpenerBackend {
    /// Same list as the Rust `DEFAULT_ALLOWED_SCHEMES`.
    public static let defaultAllowedSchemes: Set<String> = [
        "http", "https", "mailto", "tel", "sms", "geo",
    ]

    private let allowedSchemes: Set<String>

    public init(allowedSchemes: Set<String> = OpenerBackendImpl.defaultAllowedSchemes) {
        self.allowedSchemes = Set(allowedSchemes.map { $0.lowercased() })
    }

    public func open(url: String) async throws {
        // `controlCharacters` is general categories Cc *and* Cf, so this
        // also refuses invisible format characters (zero-width spaces,
        // bidi overrides and isolates, BOM) that make a URL read
        // differently from where it goes.
        let hasControl = url.unicodeScalars.contains { CharacterSet.controlCharacters.contains($0) }
        guard !url.isEmpty, !url.hasPrefix("-"), !hasControl,
              let target = URL(string: url), let scheme = target.scheme?.lowercased()
        else {
            throw OpenError.invalid
        }
        guard allowedSchemes.contains(scheme) else {
            throw OpenError.schemeNotAllowed(scheme)
        }
        #if canImport(UIKit)
        let opened = await withCheckedContinuation { cont in
            DispatchQueue.main.async {
                UIApplication.shared.open(target, options: [:]) { ok in cont.resume(returning: ok) }
            }
        }
        if !opened {
            throw OpenError.noHandler
        }
        #else
        throw OpenError.noHandler
        #endif
    }
}
