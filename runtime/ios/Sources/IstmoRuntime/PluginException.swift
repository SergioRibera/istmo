import Foundation

public struct PluginException: Error {
    public let payload: Data
    public init(payload: Data) {
        self.payload = payload
    }
}

public enum IstmoRuntimeError: Error {

    case startFailed

    case shutdown

    /// The call was cancelled through `IstmoRuntime.cancel(pluginId:)`.
    case cancelled

    case streamCancelled

    case streamError(Data)

    case unknownStream(UInt64)
}

