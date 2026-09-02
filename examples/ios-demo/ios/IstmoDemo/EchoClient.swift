// Hand-written Swift client for the Rust-hosted `Echo` plugin.
//
// This is what `istmo-build`'s `generate_swift_client` emits for a `hosts:`
// entry — an async fn per trait method that bincode-encodes arguments,
// ships them across the frame protocol via `IstmoRuntime.call`, and decodes
// the response. `EchoError` on the Rust side surfaces here as
// [EchoException].
//
// TODO: promote this file to codegen output (`istmo-build` should emit it
// from `Contract` metadata) once the generator is wired into a build
// phase.

import Foundation
import IstmoRuntime

public struct EchoClient {

    public static let PLUGIN_ID: String = "dev.istmo.demo.ios.echo"

    public init() {}

    public func echo(_ text: String) async throws -> String {
        var payload = Data()
        Bincode.writeString(&payload, text)
        do {
            let bytes = try await IstmoRuntime.shared.call(
                pluginId: Self.PLUGIN_ID,
                method: "echo",
                payload: payload
            )
            var cursor = Bincode.Cursor(bytes)
            return try Bincode.readString(&cursor)
        } catch let e as PluginException {
            // `EchoError` is `struct { reason: String }` on the Rust side.
            var cursor = Bincode.Cursor(e.payload)
            let reason = (try? Bincode.readString(&cursor)) ?? "<undecodable>"
            throw EchoException(reason: reason)
        }
    }
}

public struct EchoException: Error {
    public let reason: String
}
