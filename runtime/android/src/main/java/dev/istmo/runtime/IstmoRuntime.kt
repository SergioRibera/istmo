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

object IstmoRuntime {

    private const val NO_INSTANCE_ID = 0L
    private const val SAFE_AREA_CHANNEL = "istmo.safe_area"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()

    private val handleOwners = ConcurrentHashMap<Long, String>()
    private val nextGlobalHandleId = AtomicLong(1)

    private val outboundCalls = ConcurrentHashMap<Long, PendingCall>()
    private val nextCallId = AtomicLong(1)

    fun registerHandler(pluginId: String, handler: PluginHandler) {
        handlers[pluginId] = handler
    }

    fun allocHandleId(pluginId: String): Long {
        val id = nextGlobalHandleId.getAndIncrement()
        handleOwners[id] = pluginId
        return id
    }

    fun forgetHandle(handleId: Long) {
        handleOwners.remove(handleId)
    }

    fun start(libraryName: String): Boolean {
        System.loadLibrary(libraryName)
        return nativeStart(IstmoRuntime::class.java)
    }

    fun start(): Boolean = nativeStart(IstmoRuntime::class.java)

    fun shutdown() {
        nativeShutdown()
        scope.cancel()
        jobs.clear()
        for ((_, pending) in outboundCalls) {
            pending.cont.cancel()
        }
        outboundCalls.clear()
    }

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

    suspend fun call(pluginId: String, method: String, payload: ByteArray): ByteArray =
        call(pluginId, NO_INSTANCE_ID, method, payload)

    fun publishSafeArea(
        systemBarTop: Float, systemBarRight: Float, systemBarBottom: Float, systemBarLeft: Float,
        imeTop: Float, imeRight: Float, imeBottom: Float, imeLeft: Float,
        cutoutTop: Float, cutoutRight: Float, cutoutBottom: Float, cutoutLeft: Float,
    ) {
        val out = java.io.ByteArrayOutputStream(48)
        Bincode.writeF32(out, systemBarTop)
        Bincode.writeF32(out, systemBarRight)
        Bincode.writeF32(out, systemBarBottom)
        Bincode.writeF32(out, systemBarLeft)
        Bincode.writeF32(out, imeTop)
        Bincode.writeF32(out, imeRight)
        Bincode.writeF32(out, imeBottom)
        Bincode.writeF32(out, imeLeft)
        Bincode.writeF32(out, cutoutTop)
        Bincode.writeF32(out, cutoutRight)
        Bincode.writeF32(out, cutoutBottom)
        Bincode.writeF32(out, cutoutLeft)
        nativeSubmitEarlyLatest(SAFE_AREA_CHANNEL, out.toByteArray())
    }

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
    fun onCreateInstance(callId: Long, pluginId: String, payload: ByteArray) {
        val handler = handlers[pluginId]
        if (handler == null) {
            nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            return
        }
        val job = scope.launch {
            try {
                val instanceIdBytes = handler.handleCreateInstance(payload)
                nativeSubmitResponse(callId, true, instanceIdBytes)
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
    @Suppress("UNUSED_PARAMETER")
    fun onDestroyInstance(instanceId: Long) {

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

    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) {

    }

    @JvmStatic
    fun onReleaseNativeHandle(handleId: Long) {

        val ownerId = handleOwners.remove(handleId) ?: return
        (handlers[ownerId] as? HandleReleaser)?.releaseNativeHandle(handleId)
    }

    @JvmField
    var remoteEnvelopeSink: ((ByteArray) -> Unit)? = null

    @JvmStatic
    fun onRemoteEnvelope(bytes: ByteArray) {
        val sink = remoteEnvelopeSink
        if (sink != null) {
            sink(bytes)
        }
    }

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

    external fun nativeInjectEnvelope(bytes: ByteArray)
    external fun nativeShutdown()

    private val EMPTY_PAYLOAD = ByteArray(0)

    private class PendingCall(val cont: CancellableContinuation<ByteArray>)
}

