// Shared Swift-side surface for plugin dispatcher classes emitted by
// `istmo-build::generate_swift_host`.
//
// Every generated `<T>Dispatcher` conforms to [`PluginHandler`]. The
// runtime looks up dispatchers in [`IstmoRuntime.shared`]'s handler map
// on inbound `Frame::Call` / `Frame::CreateInstance`, calls into them
// asynchronously, and pipes the return bytes through
// `istmo_ios_submit_response`.
//
// `PluginRuntimeError` is thrown when an inbound call names an unknown
// method or instance id — a routing bug on the Rust side, not a plugin
// domain error. Wire-visible errors go through [`PluginException`].

import Foundation

public protocol PluginHandler: AnyObject {
    /// Handle a `Frame::Call` for this plugin.
    ///
    /// `instanceId` is `0` for stateless plugins. Stateful plugins
    /// (`#[istmo::plugin(init = Config)]`) use it to look up the backend
    /// registered under that id in their internal registry.
    func handleCall(instanceId: UInt64, method: String, payload: Data) async throws -> Data

    /// Handle a `Frame::CreateInstance`. Only stateful plugins override
    /// the default; the default refuses via [`PluginRuntimeError`].
    func handleCreateInstance(payload: Data) async throws -> Data
}

public extension PluginHandler {
    func handleCreateInstance(payload: Data) async throws -> Data {
        throw PluginRuntimeError.notStateful(plugin: String(describing: type(of: self)))
    }
}

/// Infrastructure-level errors dispatchers surface. Never seen by a
/// well-behaved caller; a `PluginRuntimeError` on the wire is always a
/// bug (Rust called a method the Swift backend doesn't know about, or
/// referenced an instance id that was never allocated).
public enum PluginRuntimeError: Error {
    case unknownMethod(plugin: String, method: String)
    case unknownInstance(UInt64)
    case notStateful(plugin: String)
}

/// Marker protocol a [`PluginHandler`] implements when it owns native
/// objects Rust references through `NativeHandle<T>`. The runtime routes
/// inbound `Frame::ReleaseNativeHandle` frames to [`releaseNativeHandle`]
/// on the plugin id that allocated the id via
/// [`IstmoRuntime.allocHandleId(pluginId:)`].
///
/// Mirrors Android's `HandleReleaser` interface — the wire schema is
/// symmetric across transports.
public protocol HandleReleaser: AnyObject {
    func releaseNativeHandle(_ handleId: UInt64)
}
