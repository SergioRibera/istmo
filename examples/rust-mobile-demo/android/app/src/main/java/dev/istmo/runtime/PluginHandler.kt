package dev.istmo.runtime

/**
 * Backend contract implemented by Kotlin plugins.
 *
 * `payload` is the bincode-encoded tuple of method arguments; the returned
 * `ByteArray` is the bincode-encoded return value. See [Bincode] for helpers
 * covering the primitive shapes this demo needs.
 */
interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray

    /**
     * Handle a `CreateInstance` frame. Default implementation refuses —
     * override for stateful plugins (`#[istmo::plugin(init = Cfg)]`). Return
     * the bincode-encoded `InstanceId` (a single u64 varint).
     */
    suspend fun handleCreateInstance(payload: ByteArray): ByteArray =
        throw PluginException(ByteArray(0))
}

/**
 * Throw to surface a typed domain error to the Rust caller. The `payload`
 * bytes land in `IstmoError::PluginError { bytes }` on the client side and
 * decode back into the plugin's own error enum.
 */
class PluginException(val payload: ByteArray) : RuntimeException()
