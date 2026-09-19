package dev.istmo.runtime

interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray
}

class PluginException(val payload: ByteArray) : RuntimeException()

