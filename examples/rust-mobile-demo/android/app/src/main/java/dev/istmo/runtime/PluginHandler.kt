package dev.istmo.runtime

/**
 * Wire alias for the `NativeHandleId` newtype in `istmo-core`. On the
 * Kotlin side we treat it as a plain `Long`; on the wire it is a single
 * u64 varint. Codegen dispatchers name the alias directly so the
 * generated signatures stay 1:1 with the Rust source.
 */
typealias NativeHandleId = Long

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

/**
 * Codegen-friendly exception the generated dispatchers catch.
 *
 * Backends throw `BackendException(typedError)` to surface a domain
 * error; the dispatcher catches, encodes the typed value via the plugin's
 * `<T>Codecs.write<Error>` helper, and rethrows as a [PluginException]
 * carrying the encoded bytes.
 *
 * Cleaner than making every backend hand-encode + throw `PluginException`
 * directly — the encode step lives in the codec impl, not scattered
 * across the backend.
 */
class BackendException<E : Any>(val error: E) : RuntimeException()

/**
 * Marker interface a [PluginHandler] implements when it owns native
 * objects referenced from Rust through `NativeHandle<T>`. The runtime
 * routes `Frame::ReleaseNativeHandle` frames to
 * [releaseNativeHandle] on the plugin id that allocated the id via
 * [IstmoRuntime.allocHandleId].
 */
interface HandleReleaser {
    fun releaseNativeHandle(handleId: Long)
}
