package dev.istmo.runtime

import java.util.concurrent.ConcurrentHashMap
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Kotlin-side singleton for the istmo runtime.
 *
 * The Rust pump thread invokes the `@JvmStatic` callbacks below with typed
 * arguments for each outbound frame variant. Kotlin submits inbound frames
 * (responses, events, stream ends) through the `nativeSubmit*` methods, which
 * are trampolines exported by the `istmo-android` crate.
 *
 * The dynamic library name is fixed per demo cdylib; adjust `LIBRARY_NAME` if
 * this runtime is embedded into a differently-named `.so`.
 */
object IstmoRuntime {

    private const val LIBRARY_NAME = "istmo_android_demo"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()

    init {
        System.loadLibrary(LIBRARY_NAME)
    }

    /** Register a Kotlin backend for a plugin id. */
    fun registerHandler(pluginId: String, handler: PluginHandler) {
        handlers[pluginId] = handler
    }

    /**
     * Initialise the process runtime. Safe to call at most once per process;
     * subsequent calls return `false`.
     */
    fun start(): Boolean = nativeStart(IstmoRuntime::class.java)

    /** Cancel every pending call/stream and stop the pump thread. */
    fun shutdown() {
        nativeShutdown()
        scope.cancel()
        jobs.clear()
    }

    // ---- Callbacks from the Rust pump thread ----

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
            } catch (e: Throwable) {
                // Unhandled backend failure: surface as empty error payload;
                // the client will see IstmoError::PluginError { bytes: [] }.
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

    // ---- Trampolines exported by istmo-android ----

    external fun nativeStart(runtimeClass: Class<*>): Boolean
    external fun nativeSubmitResponse(callId: Long, ok: Boolean, payload: ByteArray)
    external fun nativeSubmitEvent(streamId: Long, payload: ByteArray)
    external fun nativeSubmitStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray?)
    external fun nativeShutdown()

    private val EMPTY_PAYLOAD = ByteArray(0)
}
