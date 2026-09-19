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

            var cursor = Bincode.Cursor(e.payload)
            let reason = (try? Bincode.readString(&cursor)) ?? "<undecodable>"
            throw EchoException(reason: reason)
        }
    }
}

public struct EchoException: Error {
    public let reason: String
}

