package dev.istmo.runtime

import android.app.Service
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
 * Multi-cdylib demo note: `LIBRARY_NAME` names the app cdylib. Additional
 * cdylibs packaged into the same APK (e.g. `istmo_android_multi_widget`)
 * would each need their own JNI class + runtime; that's deferred until the
 * widget process lands. For this demo only the app cdylib is loaded.
 */
object IstmoRuntime {

    private const val LIBRARY_NAME = "istmo_android_multi_app"
    private const val NO_INSTANCE_ID = 0L

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()
    private val services = ConcurrentHashMap<String, Service>()

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

    /**
     * Register a running service instance under its plugin id. Consumed by
     * `ServiceControlImpl` to reach the correct `Service.startForeground` /
     * `stopSelf` receiver.
     */
    fun registerService(pluginId: String, service: Service) {
        services[pluginId] = service
    }

    fun unregisterService(pluginId: String) {
        services.remove(pluginId)
    }

    fun service(pluginId: String): Service? = services[pluginId]

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
    fun onNotify(
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ) {
        val handler = handlers[pluginId] ?: return
        scope.launch {
            runCatching { handler.handleCall(instanceId, method, payload) }
        }
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onCreateInstance(callId: Long, pluginId: String, payload: ByteArray) {
        // No stateful plugins in this demo.
        nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
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
    @Suppress("UNUSED_PARAMETER")
    fun onEvent(streamId: Long, payload: ByteArray) {
        // Kotlin-consumed streams aren't exercised by this demo.
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) {
        // See onEvent.
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onReleaseNativeHandle(handleId: Long) {
        // No native handle registry in this demo. Real apps free the object
        // stored under handleId from their per-plugin table here.
    }

    /**
     * Sink for `:remote`-bridge envelope bytes. See
     * `docs/remote-bridge/` for the reference wiring; the demo leaves it
     * `null` and drops any remote envelope with a log line.
     */
    @JvmField
    var remoteEnvelopeSink: ((ByteArray) -> Unit)? = null

    @JvmStatic
    fun onRemoteEnvelope(bytes: ByteArray) {
        val sink = remoteEnvelopeSink
        if (sink != null) {
            sink(bytes)
        }
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
    external fun nativeSubmitNotify(
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
    /** Receive-side of a `:remote` bridge — bytes shipped over Binder. */
    external fun nativeInjectEnvelope(bytes: ByteArray)
    external fun nativeShutdown()

    private val EMPTY_PAYLOAD = ByteArray(0)

    private class PendingCall(val cont: CancellableContinuation<ByteArray>)
}
