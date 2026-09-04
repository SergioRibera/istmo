import Foundation

/// Domain-error escape hatch: what `IstmoRuntime.call(...)` throws when the
/// Rust side responds with `Frame::Respond { result: Err(bytes) }`. The
/// bytes are the plugin's opaque error payload — decoding them into the
/// declared error type is the caller's responsibility (usually via the
/// generated `<Plugin>Client` wrapper).
public struct PluginException: Error {
    public let payload: Data
    public init(payload: Data) {
        self.payload = payload
    }
}

/// Infrastructure failures raised by the runtime itself. Distinct from
/// [`PluginException`] because these have no plugin-level meaning — they
/// mean the transport / runtime / codec broke.
public enum IstmoRuntimeError: Error {
    /// `IstmoRuntime.start()` failed. See device console for the underlying
    /// tracing message; the C side logs before returning `false`.
    case startFailed
    /// The runtime was shut down while a call was in flight.
    case shutdown
    /// A Rust-hosted stream ended with `StreamEndReason::Cancelled` before
    /// the last event was consumed.
    case streamCancelled
    /// A Rust-hosted stream ended with `StreamEndReason::Error(bytes)`.
    case streamError(Data)
    /// Something claimed a stream event but no stream was registered under
    /// that id — signals a routing bug on the Rust side.
    case unknownStream(UInt64)
}
