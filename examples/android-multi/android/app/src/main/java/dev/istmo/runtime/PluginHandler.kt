package dev.istmo.runtime

typealias NativeHandleId = Long

interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray

    suspend fun handleCreateInstance(payload: ByteArray): ByteArray =
        throw PluginException(ByteArray(0))
}

class PluginException(val payload: ByteArray) : RuntimeException()

class BackendException(val error: Any) : RuntimeException()

interface HandleReleaser {
    fun releaseNativeHandle(handleId: Long)
}

