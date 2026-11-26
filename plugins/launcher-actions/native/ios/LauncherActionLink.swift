import Foundation
import IstmoRuntime

/// The deep link a launcher action publishes. Foundation-only so its bytes
/// can be checked off-device.
public enum LauncherActionLink {
    /// Mirrors `istmo_plugins::DEEPLINKS_CHANNEL`.
    public static let deepLinksChannel = "istmo.deeplinks"
    /// Mirrors `istmo_plugins::DEFAULT_DEEPLINKS_CAPACITY`.
    public static let deepLinksCapacity: UInt32 = 16
    /// Mirrors `istmo_launcher_actions::ACTION_SOURCE_PREFIX`.
    public static let sourcePrefix = "launcher-action:"

    /// `istmo_plugins::DeepLink { uri, source, received_at_ms }` in its
    /// wire layout (bincode 2 `standard()`): a string, an optional string,
    /// an optional varint `u64`. Pinned by `tests/deep_link_wire.rs`.
    public static func encode(uri: String, source: String?, receivedAtMs: UInt64?) -> Data {
        var out = Data()
        Bincode.writeString(&out, uri)
        if let source = source {
            Bincode.writeU8(&out, 1)
            Bincode.writeString(&out, source)
        } else {
            Bincode.writeU8(&out, 0)
        }
        if let ms = receivedAtMs {
            Bincode.writeU8(&out, 1)
            Bincode.writeVarintU64(&out, ms)
        } else {
            Bincode.writeU8(&out, 0)
        }
        return out
    }
}
