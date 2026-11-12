import Foundation

/// Wire form of a native handle: the bare `u64` Rust's `NativeHandleId`
/// wraps. Generated codecs read and write it as a varint.
public typealias NativeHandleId = UInt64

public protocol PluginHandler: AnyObject {

    func handleCall(instanceId: UInt64, method: String, payload: Data) async throws -> Data

    func handleCreateInstance(payload: Data) async throws -> Data

    /// Fire-and-forget call from Rust (`Runtime::notify`): no response
    /// is sent back, so failures are only logged.
    func handleNotify(instanceId: UInt64, method: String, payload: Data) async
}

public extension PluginHandler {
    func handleCreateInstance(payload: Data) async throws -> Data {
        throw PluginRuntimeError.notStateful(plugin: String(describing: type(of: self)))
    }

    /// Runs the method like a call and drops the result. Unknown methods
    /// are a silent no-op, as the protocol requires for notifications.
    func handleNotify(instanceId: UInt64, method: String, payload: Data) async {
        do {
            _ = try await handleCall(instanceId: instanceId, method: method, payload: payload)
        } catch PluginRuntimeError.unknownMethod {
            return
        } catch {
            NSLog("IstmoRuntime: notify \(method) failed: \(error)")
        }
    }
}

public enum PluginRuntimeError: Error {
    case unknownMethod(plugin: String, method: String)
    case unknownInstance(UInt64)
    case notStateful(plugin: String)
}

public protocol HandleReleaser: AnyObject {
    func releaseNativeHandle(_ handleId: UInt64)
}

