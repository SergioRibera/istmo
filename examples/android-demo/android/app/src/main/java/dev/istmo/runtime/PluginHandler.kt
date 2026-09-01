package dev.istmo.runtime

/**
 * Backend contract implemented by Kotlin plugins.
 *
 * `payload` is the bincode-encoded tuple of method arguments; the returned
 * `ByteArray` is the bincode-encoded return value. See [Bincode] for helpers
 * covering the primitive shapes the demo needs.
 */
interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray
}

/**
 * Throw to surface a typed domain error to the Rust caller. The `payload`
 * bytes end up in `IstmoError::PluginError { bytes }` on the client side.
 */
class PluginException(val payload: ByteArray) : RuntimeException()
