import Foundation

public struct PluginException: Error {
    public let payload: Data
    public init(payload: Data) {
        self.payload = payload
    }
}

public enum IstmoRuntimeError: Error {

    case startFailed

    /// The linked Rust library speaks another native entry version
    /// (`istmo_abi_version`); rebuild it with the matching istmo.
    case abiMismatch(found: UInt32, expected: UInt32)

    case shutdown

    /// The call was cancelled through `IstmoRuntime.cancel(pluginId:)`.
    case cancelled

    case streamCancelled

    case streamError(Data)

    case unknownStream(UInt64)
}

