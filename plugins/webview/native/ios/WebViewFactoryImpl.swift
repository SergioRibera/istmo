import Foundation
import IstmoRuntime

/// Builds a [`WebViewBackendImpl`] per client instance.
public final class WebViewFactoryImpl: WebViewFactory {
    public init() {}

    public func create(config: WebViewInitConfig) async throws -> WebViewBackend {
        WebViewBackendImpl(config: config)
    }
}
