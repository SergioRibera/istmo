import Foundation
import IstmoRuntime
#if canImport(UIKit)
import UIKit
#endif

/// Reference backend for `istmo.opener` on top of `UIApplication.open`.
public final class OpenerBackendImpl: OpenerBackend {
    public init() {}

    public func open(url: String) async throws {
        guard let target = URL(string: url), target.scheme != nil else {
            throw OpenError.invalid
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
