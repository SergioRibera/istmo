package dev.istmo.runtime

/**
 * Backend contract implemented by Kotlin plugins. `payload` is bincoded per
 * the plugin's method contract; the returned `ByteArray` is the bincoded
 * return value. See [Bincode] for helpers.
 */
interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray
}

/**
 * Throw to surface a typed domain error to the Rust caller. The `payload`
 * bytes land in `IstmoError::PluginError { bytes }` on the client side.
 */
class PluginException(val payload: ByteArray) : RuntimeException()
