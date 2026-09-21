package dev.istmo.runtime

import kotlinx.coroutines.flow.Flow

typealias NativeHandleId = Long

/**
 * Result of [PluginHandler.handleCall]. Unary methods produce
 * [Unary], stream methods (declared with `#[stream]` on the Rust
 * side) produce [Stream] — the runtime consumes each `ByteArray`
 * emitted by the flow as a `Frame::Event` back to the caller, and
 * closes the wire stream when the flow completes or errors.
 */
sealed class PluginResult {
    class Unary(val bytes: ByteArray) : PluginResult()
    class Stream(val flow: Flow<ByteArray>) : PluginResult()
}

interface PluginHandler {
    suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): PluginResult

    suspend fun handleCreateInstance(payload: ByteArray): ByteArray =
        throw PluginException(ByteArray(0))
}

class PluginException(val payload: ByteArray) : RuntimeException()

class BackendException(val error: Any) : RuntimeException()

interface HandleReleaser {
    fun releaseNativeHandle(handleId: Long)
}
