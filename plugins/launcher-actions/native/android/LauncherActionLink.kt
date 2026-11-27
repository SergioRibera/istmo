package dev.istmo.plugins.launcher_actions

import dev.istmo.runtime.Bincode
import java.io.ByteArrayOutputStream

/**
 * The deep link a launcher action publishes. Plain JVM code (no Android
 * types) so its bytes can be checked off-device.
 */
object LauncherActionLink {
    /** Mirrors `istmo_plugins::DEEPLINKS_CHANNEL`. */
    const val DEEPLINKS_CHANNEL = "istmo.deeplinks"

    /** Mirrors `istmo_plugins::DEFAULT_DEEPLINKS_CAPACITY`. */
    const val DEEPLINKS_CAPACITY = 16

    /** Mirrors `istmo_launcher_actions::ACTION_SOURCE_PREFIX`. */
    const val SOURCE_PREFIX = "launcher-action:"

    /**
     * `istmo_plugins::DeepLink { uri, source, received_at_ms }` in its wire
     * layout (bincode 2 `standard()`): a string, an optional string, an
     * optional varint `u64`. Pinned by `tests/deep_link_wire.rs`.
     */
    @JvmStatic
    fun encode(uri: String, source: String?, receivedAtMs: Long?): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, uri)
        Bincode.writeOption(out, source) { sink, s -> Bincode.writeString(sink, s) }
        Bincode.writeOption(out, receivedAtMs) { sink, ms -> Bincode.writeVarintU64(sink, ms) }
        return out.toByteArray()
    }
}
