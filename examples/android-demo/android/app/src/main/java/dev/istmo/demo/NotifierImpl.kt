package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.PluginHandler

/**
 * Kotlin backend for the `dev.istmo.demo.notifier` plugin.
 *
 * Every inbound `notify(String)` is a Rust-initiated crossing —
 * Rust's `EchoImpl::spam_notify` sends N of them in a row. The handler
 * appends each message to a caller-provided sink so the UI can show the
 * cascade. Return type is `Result<(), EchoError>`; the Ok payload is a
 * zero-byte bincode encoding of `()`.
 */
class NotifierImpl(private val sink: (String) -> Unit) : PluginHandler {

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "notify" -> {
            val (message, _) = Bincode.readString(payload)
            sink(message)
            EMPTY_UNIT
        }
        else -> error("unknown Notifier method: $method")
    }

    private companion object {
        val EMPTY_UNIT = ByteArray(0)
    }
}
