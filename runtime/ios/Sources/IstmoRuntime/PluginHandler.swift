import Foundation

/// Wire form of a native handle: the bare `u64` Rust's `NativeHandleId`
/// wraps. Generated codecs read and write it as a varint.
public typealias NativeHandleId = UInt64

/// Result of `PluginHandler.handleCall`. Unary methods produce `.unary`,
/// stream methods (declared with `#[stream]` on the Rust side) produce
/// `.stream`: the runtime sends each element as a `Frame::Event` back to
/// the caller and ends the wire stream when the sequence finishes or
/// throws (a `PluginException` ends it with its payload as the error).
public enum PluginResult {
    case unary(Data)
    case stream(AsyncThrowingStream<Data, Error>)

    /// Encodes every item of a backend stream. `mapError` turns a thrown
    /// domain error into the `PluginException` carrying its encoding.
    /// Terminating the returned stream cancels the backend's.
    public static func encoding<Item>(
        _ source: AsyncThrowingStream<Item, Error>,
        encode: @escaping (Item) throws -> Data,
        mapError: @escaping (Error) -> Error = { $0 }
    ) -> PluginResult {
        .stream(AsyncThrowingStream { cont in
            let pump = Task {
                do {
                    for try await item in source {
                        cont.yield(try encode(item))
                    }
                    cont.finish()
                } catch {
                    cont.finish(throwing: mapError(error))
                }
            }
            cont.onTermination = { _ in pump.cancel() }
        })
    }
}

public protocol PluginHandler: AnyObject {

    func handleCall(instanceId: UInt64, method: String, payload: Data) async throws -> PluginResult

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

