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

    /** Early events published before the native library was loaded. */
    private val pendingEarlyQueue = java.util.ArrayDeque<Triple<String, Int, ByteArray>>()

    fun registerHandler(pluginId: String, handler: PluginHandler) {
        handlers[pluginId] = handler
    }

    /**
     * Drop the handler registered for [pluginId]; later calls to it fail
     * as for an unregistered plugin. Calls already running keep their
     * handler until they finish.
     */
    fun unregisterHandler(pluginId: String) {
        handlers.remove(pluginId)
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
        return start()
    }

    /**
     * Preferred entry point: loads the Rust cdylib, starts the runtime
     * and immediately wires the platform Context so `istmo::assets` and
     * `istmo::path` are usable from the first call. Equivalent to
     * calling [start], then [installContext].
     */
    fun start(context: android.content.Context, libraryName: String): Boolean {
        val ok = start(libraryName)
        if (ok) installContext(context)
        return ok
    }

    /**
     * Start the Rust runtime and its pump thread.
     *
     * Idempotent while the runtime runs: later calls (another activity, a
     * service in the same process) return `true` without restarting it.
     * Always returns `true`; failures throw [IllegalStateException]
     * (or the underlying JNI error, e.g. [NoSuchMethodError] when this
     * class and the native library disagree on a callback signature).
     * After [shutdown] the runtime cannot be started again in the same
     * process.
     */
    fun start(): Boolean {
        val ok = nativeStart(IstmoRuntime::class.java)
        synchronized(pendingEarlyQueue) { flushPendingEarlyQueueLocked() }
        return ok
    }

    /**
     * Attach the app's [android.content.Context] so `istmo::assets` and
     * `istmo::path` can serve reads from the APK's asset table and the
     * per-app directories. Call once on startup, typically from
     * `Application.onCreate` right after [start].
     */
    fun installContext(context: android.content.Context) {
        nativeInstallAssets(context.assets)
        val external = context.getExternalFilesDir(null)?.absolutePath ?: ""
        nativeInstallPaths(
            context.filesDir.absolutePath,
            context.cacheDir.absolutePath,
            context.noBackupFilesDir.absolutePath,
            external,
        )
    }

    /**
     * Publish [payload] on the early-event queue [channel] (see
     * `istmo_core::early_events::PreMainQueue`). Safe to call before the
     * native library is loaded — e.g. from an `Activity.onCreate` that
     * handles a share intent — the event is held here and flushed on the
     * next publish or on [start]. Once the library is loaded, the Rust
     * side buffers until the runtime itself is initialised.
     */
    fun publishEarlyQueue(channel: String, capacity: Int, payload: ByteArray) {
        synchronized(pendingEarlyQueue) {
            pendingEarlyQueue.addLast(Triple(channel, capacity, payload))
            flushPendingEarlyQueueLocked()
        }
    }

    private fun flushPendingEarlyQueueLocked() {
        while (pendingEarlyQueue.isNotEmpty()) {
            val (channel, capacity, payload) = pendingEarlyQueue.peekFirst()
            try {
                nativeSubmitEarlyQueue(channel, capacity, payload)
            } catch (_: UnsatisfiedLinkError) {
                return
            }
            pendingEarlyQueue.removeFirst()
        }
    }

    /**
     * Cancel pending work and stop the pump. Terminal for the process:
     * the native runtime lives in a process-wide `OnceLock`, so a later
     * [start] throws instead of restarting it. Idempotent.
     */
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
            // Cancellation handlers must not throw; a cancel racing
            // shutdown has nothing left to stop anyway.
            if (outboundCalls.remove(callId) != null) {
                runCatching { nativeSubmitCancel(callId) }
            }
        }
        try {
            nativeSubmitCall(callId, pluginId, instanceId, method, payload)
        } catch (e: Throwable) {
            // Not only IllegalStateException: a pending JNI error (e.g.
            // OutOfMemoryError) or UnsatisfiedLinkError must not leak the
            // pending entry either.
            outboundCalls.remove(callId)
            throw e
        }
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
                when (result) {
                    is PluginResult.Unary -> nativeSubmitResponse(callId, true, result.bytes)
                    is PluginResult.Stream -> pumpStream(callId, result.flow)
                }
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

    private suspend fun pumpStream(callId: Long, flow: kotlinx.coroutines.flow.Flow<ByteArray>) {
        try {
            // `Unconfined` keeps the collector on the thread that emitted
            // the item instead of hopping onto a `Dispatchers.Default`
            // worker per sample. For high-rate streams (stylus input, pen
            // hover) the per-event scheduler hop was adding tens of ms of
            // visible latency on Android — the native-side submit is a
            // short JNI + bincode push that is safe to run inline from
            // the emitter's thread. Downstream consumers already run on
            // dedicated Rust OS threads, so the native call never blocks
            // on UI work.
            kotlinx.coroutines.withContext(kotlinx.coroutines.Dispatchers.Unconfined) {
                flow.collect { bytes -> nativeSubmitEvent(callId, bytes) }
            }
            nativeSubmitStreamEnd(callId, STREAM_END_COMPLETE, null)
        } catch (e: kotlinx.coroutines.CancellationException) {
            nativeSubmitStreamEnd(callId, STREAM_END_CANCELLED, null)
            throw e
        } catch (e: PluginException) {
            // Generated dispatchers turn a domain error into its encoded bytes.
            nativeSubmitStreamEnd(callId, STREAM_END_ERROR, e.payload)
        } catch (_: Throwable) {
            nativeSubmitStreamEnd(callId, STREAM_END_CANCELLED, null)
        }
    }

    private const val STREAM_END_COMPLETE = 0
    private const val STREAM_END_CANCELLED = 1
    private const val STREAM_END_ERROR = 2

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

    // Every `native*` below throws IllegalStateException when the call
    // points at a bug (runtime not started, malformed bytes, bad
    // argument). Late frames racing a cancel or shutdown are dropped
    // silently on the Rust side.
    external fun nativeStart(runtimeClass: Class<*>): Boolean
    external fun nativeSubmitCancel(callId: Long)
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
    external fun nativeInstallAssets(assetManager: android.content.res.AssetManager)
    external fun nativeInstallPaths(
        filesDir: String,
        cacheDir: String,
        noBackupFilesDir: String,
        externalFilesDir: String,
    )
    external fun nativeShutdown()

    private val EMPTY_PAYLOAD = ByteArray(0)

    private class PendingCall(val cont: CancellableContinuation<ByteArray>)
}

