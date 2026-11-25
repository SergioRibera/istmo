package dev.istmo.bench

import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PluginHandler
import dev.istmo.runtime.PluginResult
import java.util.concurrent.atomic.AtomicLongArray

/**
 * Kotlin-side stage stamps (`System.nanoTime()`) of the call in flight.
 * The benchmark drives one probed call at a time, so each slot holds the
 * last time its stage was reached.
 */
object Probe {
    const val ON_CALL_ENTER = 0
    const val ON_CALL_EXIT = 1
    const val HANDLER_ENTER = 2
    const val BACKEND_ENTER = 3
    const val BACKEND_EXIT = 4
    const val HANDLER_EXIT = 5
    const val ON_RESPOND_ENTER = 6
    const val ON_RESPOND_EXIT = 7
    const val ON_EVENT_ENTER = 8
    private const val COUNT = 9

    @Volatile
    var enabled = false

    val stamps = AtomicLongArray(COUNT)

    fun stamp(slot: Int) {
        if (enabled) stamps.set(slot, System.nanoTime())
    }

    fun read(slot: Int): Long = stamps.get(slot)

    /** Wait (up to 2 ms) until [after] is at least as recent as [before]: "exit" stamps land after the hand-off. */
    fun settle(before: Int, after: Int) {
        val deadline = System.nanoTime() + 2_000_000
        while (read(after) < read(before) && System.nanoTime() < deadline) { // spin
        }
    }
}

/**
 * Stand-in for `IstmoRuntime` as the pump's callback class: the runtime
 * resolves the pump callbacks on whatever class `nativeStart` receives,
 * so the probe run passes this one. Every callback stamps, then forwards
 * to the real `IstmoRuntime` handler, so the runtime itself is unchanged.
 */
object ProbeRuntime {
    @JvmStatic
    fun onCall(callId: Long, pluginId: String, instanceId: Long, method: String, payload: ByteArray) {
        Probe.stamp(Probe.ON_CALL_ENTER)
        IstmoRuntime.onCall(callId, pluginId, instanceId, method, payload)
        Probe.stamp(Probe.ON_CALL_EXIT)
    }

    @JvmStatic
    fun onCancel(callId: Long) = IstmoRuntime.onCancel(callId)

    @JvmStatic
    fun onCreateInstance(callId: Long, pluginId: String, payload: ByteArray) =
        IstmoRuntime.onCreateInstance(callId, pluginId, payload)

    @JvmStatic
    fun onDestroyInstance(instanceId: Long) = IstmoRuntime.onDestroyInstance(instanceId)

    @JvmStatic
    fun onRespond(callId: Long, ok: Boolean, payload: ByteArray) {
        Probe.stamp(Probe.ON_RESPOND_ENTER)
        IstmoRuntime.onRespond(callId, ok, payload)
        Probe.stamp(Probe.ON_RESPOND_EXIT)
    }

    @JvmStatic
    fun onEvent(streamId: Long, payload: ByteArray) {
        Probe.stamp(Probe.ON_EVENT_ENTER)
        IstmoRuntime.onEvent(streamId, payload)
    }

    @JvmStatic
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) =
        IstmoRuntime.onStreamEnd(streamId, reason, errorPayload)

    @JvmStatic
    fun onReleaseNativeHandle(handleId: Long) = IstmoRuntime.onReleaseNativeHandle(handleId)

    @JvmStatic
    fun onNotify(pluginId: String, instanceId: Long, method: String, payload: ByteArray) =
        IstmoRuntime.onNotify(pluginId, instanceId, method, payload)

    @JvmStatic
    fun onRemoteEnvelope(bytes: ByteArray) = IstmoRuntime.onRemoteEnvelope(bytes)
}

/** Stamps around a generated dispatcher: entry after the coroutine hop, exit after the reply encode. */
class ProbeHandler(private val inner: PluginHandler) : PluginHandler {
    override suspend fun handleCall(instanceId: Long, method: String, payload: ByteArray): PluginResult {
        Probe.stamp(Probe.HANDLER_ENTER)
        val result = inner.handleCall(instanceId, method, payload)
        Probe.stamp(Probe.HANDLER_EXIT)
        return result
    }
}
