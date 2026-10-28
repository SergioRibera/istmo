import Foundation

/// Factory the host runtime registers for `istmo.camera`. Mirror of
/// `CameraFactoryImpl` on Android — kept trivial on purpose so apps
/// can override one half without touching the other.
final class CameraFactoryImpl: CameraFactory {
    func create(config: CameraInitConfig) async throws -> CameraBackend {
        CameraBackendImpl(config: config)
    }
}
