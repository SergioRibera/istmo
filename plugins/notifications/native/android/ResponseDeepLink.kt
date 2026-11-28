package dev.istmo.plugins.notifications

import dev.istmo.runtime.Bincode
import java.io.ByteArrayOutputStream

/**
 * The `DeepLink { uri, source, received_at_ms }` a response publishes on
 * `istmo.deeplinks`, the port of `NotificationResponse::deep_link` in
 * `src/responses.rs`. Plain Kotlin so the JVM checks run it.
 */
internal object ResponseDeepLink {
    /**
     * Wire bytes of the deep link of [response], received at [receivedAtMs]:
     * only taps and buttons with a non-empty URL publish one (`Dismiss` and
     * `More` never do; an empty URL opens nothing). Source
     * `notification:<id>` or `notification:<id>:<action>`.
     */
    fun encode(response: NotificationResponse, receivedAtMs: Long): ByteArray? {
        val url = response.url?.takeIf { it.isNotEmpty() } ?: return null
        val source = when (val action = response.action) {
            is ResponseAction.Open -> "notification:${response.id.value}"
            is ResponseAction.Button -> "notification:${response.id.value}:${action.value.value}"
            else -> return null
        }
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, url)
        Bincode.writeOption(out, source) { sink, s -> Bincode.writeString(sink, s) }
        Bincode.writeOption(out, receivedAtMs) { sink, ms -> Bincode.writeVarintU64(sink, ms) }
        return out.toByteArray()
    }
}
