package dev.istmo.runtime

/**
 * Wire alias for the `NativeHandleId` newtype in `istmo-core`. On the
 * Kotlin side it is a plain `Long`; on the wire it is a single u64
 * varint.
 */
typealias NativeHandleId = Long

/**
 * Backend contract implemented by Kotlin plugins. `payload` is bincoded per
 * the plugin's method contract; the returned `ByteArray` is the bincoded
 * return value. See [Bincode] for helpers.
 */
interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): ByteArray

    /**
     * Handle a `CreateInstance` frame. Default refuses — override for
     * stateful plugins (`#[istmo::plugin(init = Cfg)]`). Return the
     * bincode-encoded `InstanceId` (u64 varint).
     */
    suspend fun handleCreateInstance(payload: ByteArray): ByteArray =
        throw PluginException(ByteArray(0))
}

/**
 * Throw to surface a typed domain error to the Rust caller. The `payload`
 * bytes land in `IstmoError::PluginError { bytes }` on the client side.
 */
class PluginException(val payload: ByteArray) : RuntimeException()

/**
 * Codegen-friendly exception the generated dispatchers catch. Backends
 * throw `BackendException(typedError)` to surface a domain error; the
 * dispatcher safe-casts `error` to the plugin's concrete error type,
 * encodes via `<T>Codecs.write<Error>` and rethrows as [PluginException]
 * carrying the encoded bytes.
 *
 * The class is not generic — JVM forbids type parameters on `Throwable`
 * subclasses because exception dispatch uses the erased class hierarchy.
 */
class BackendException(val error: Any) : RuntimeException()

/**
 * Marker interface a [PluginHandler] implements when it owns native
 * objects referenced from Rust through `NativeHandle<T>`. The runtime
 * routes `Frame::ReleaseNativeHandle` frames to [releaseNativeHandle]
 * on the plugin id that allocated the id.
 */
interface HandleReleaser {
    fun releaseNativeHandle(handleId: Long)
}
