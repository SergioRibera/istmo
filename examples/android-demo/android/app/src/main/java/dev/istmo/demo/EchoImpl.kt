package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.PluginHandler

class EchoImpl : PluginHandler {
    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "echo" -> {
            val (text, _) = Bincode.readString(payload)
            Bincode.writeString("echo: $text")
        }
        else -> throw IllegalArgumentException("unknown Echo method: $method")
    }
}
