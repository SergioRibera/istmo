package dev.istmo.runtime

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * Kotlin-side singleton for the istmo runtime.
 *
 * The frame protocol is symmetric: the Rust pump thread invokes the
 * `@JvmStatic on*` callbacks below for every outbound frame (Rust → Kotlin),
 * and Kotlin submits inbound frames through the `nativeSubmit*` trampolines
 * exported by `istmo-android`.
 *
 * Two directions coexist:
 *  * Rust hosts a trait (`hosts: [T => TImpl]`) → Kotlin sends
 *    `Frame::Call` via [call] and awaits the matching `Respond` on
 *    [onRespond].
 *  * Native hosts a plugin (`plugins: [T]`) → Rust sends `Frame::Call` via
 *    the pump → Kotlin's registered [PluginHandler] answers via the
 *    existing `nativeSubmitResponse` trampoline.
 */
object IstmoRuntime {

    private const val LIBRARY_NAME = "istmo_android_demo"
    private const val NO_INSTANCE_ID = 0L

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()

    /** Pending Kotlin-initiated calls awaiting a `Frame::Respond`. */
    private val outboundCalls = ConcurrentHashMap<Long, PendingCall>()
    private val nextCallId = AtomicLong(1)

    init {
        System.loadLibrary(LIBRARY_NAME)
    }

    /** Register a Kotlin backend for a plugin id (used for `plugins:` direction). */
    fun registerHandler(pluginId: String, handler: PluginHandler) {
        handlers[pluginId] = handler
    }

    /** Initialise the process runtime. Idempotent — subsequent calls return `false`. */
    fun start(): Boolean = nativeStart(IstmoRuntime::class.java)

    /** Cancel every pending call/stream and stop the pump thread. */
    fun shutdown() {
        nativeShutdown()
        scope.cancel()
        jobs.clear()
        for ((_, pending) in outboundCalls) {
            pending.cont.cancel()
        }
        outboundCalls.clear()
    }

    // ---- Outbound (Kotlin → Rust) ---------------------------------------

    /**
     * Send a `Frame::Call` and suspend until the matching `Respond` arrives.
     * `payload` is bincode-encoded per plugin contract; the return is the
     * raw response bytes on success. Domain errors surface as
     * [PluginException] with the encoded error payload.
     */
    suspend fun call(
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = suspendCancellableCoroutine { cont ->
        val callId = nextCallId.getAndIncrement()
        outboundCalls[callId] = PendingCall(cont)
        cont.invokeOnCancellation {
            outboundCalls.remove(callId)
            nativeSubmitCall(callId, pluginId, instanceId, method, EMPTY_PAYLOAD)
        }
        nativeSubmitCall(callId, pluginId, instanceId, method, payload)
    }

    /** Convenience for stateless plugins (no instance id). */
    suspend fun call(pluginId: String, method: String, payload: ByteArray): ByteArray =
        call(pluginId, NO_INSTANCE_ID, method, payload)

    /** Publish a latest-value early event (lifecycle-shaped). */
    fun submitEarlyLatest(channel: String, payload: ByteArray) {
        nativeSubmitEarlyLatest(channel, payload)
    }

    /** Publish a queued early event (deep-link-shaped). */
    fun submitEarlyQueue(channel: String, capacity: Int, payload: ByteArray) {
        nativeSubmitEarlyQueue(channel, capacity, payload)
    }

    // ---- Callbacks from the Rust pump thread ----------------------------

    @JvmStatic
    fun onCall(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ) {
        val handler = handlers[pluginId]
        if (handler == null) {
            nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            return
        }
        val job = scope.launch {
            try {
                val result = handler.handleCall(instanceId, method, payload)
                nativeSubmitResponse(callId, true, result)
            } catch (e: PluginException) {
                nativeSubmitResponse(callId, false, e.payload)
            } catch (_: Throwable) {
                nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            } finally {
                jobs.remove(callId)
            }
        }
        jobs[callId] = job
    }

    @JvmStatic
    fun onCancel(callId: Long) {
        jobs.remove(callId)?.cancel()
    }

    @JvmStatic
    fun onCreateInstance(callId: Long, pluginId: String, payload: ByteArray) {
        // Stateful plugins are not exercised by this demo. Reply with an
        // empty error so the client's `create_instance` future resolves.
        nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
    }

    @JvmStatic
    fun onDestroyInstance(instanceId: Long) {
        // No-op: this demo never creates instances.
    }

    @JvmStatic
    fun onRespond(callId: Long, ok: Boolean, payload: ByteArray) {
        val pending = outboundCalls.remove(callId) ?: return
        if (ok) {
            pending.cont.resume(payload)
        } else {
            pending.cont.resumeWithException(PluginException(payload))
        }
    }

    @JvmStatic
    fun onEvent(streamId: Long, payload: ByteArray) {
        // Streams initiated from Kotlin aren't yet supported by this demo.
        // Log and drop.
    }

    @JvmStatic
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) {
        // Same as onEvent — no Kotlin-side streams yet.
    }

    // ---- Trampolines exported by istmo-android --------------------------

    external fun nativeStart(runtimeClass: Class<*>): Boolean
    external fun nativeSubmitCall(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    )
    external fun nativeSubmitResponse(callId: Long, ok: Boolean, payload: ByteArray)
    external fun nativeSubmitEvent(streamId: Long, payload: ByteArray)
    external fun nativeSubmitStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray?)
    external fun nativeSubmitEarlyLatest(channel: String, payload: ByteArray)
    external fun nativeSubmitEarlyQueue(channel: String, capacity: Int, payload: ByteArray)
    external fun nativeShutdown()

    private val EMPTY_PAYLOAD = ByteArray(0)

    private class PendingCall(val cont: CancellableContinuation<ByteArray>)
}
