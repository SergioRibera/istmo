import Foundation

public protocol PluginHandler: AnyObject {

    func handleCall(instanceId: UInt64, method: String, payload: Data) async throws -> Data

    func handleCreateInstance(payload: Data) async throws -> Data
}

public extension PluginHandler {
    func handleCreateInstance(payload: Data) async throws -> Data {
        throw PluginRuntimeError.notStateful(plugin: String(describing: type(of: self)))
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

