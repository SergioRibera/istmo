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
 * `LIBRARY_NAME` is the demo's own cdylib. Loading it via `System.loadLibrary`
 * both pulls the JNI trampolines in and satisfies NativeActivity's own
 * `android.app.lib_name` requirement (idempotent — subsequent loads no-op).
 */
object IstmoRuntime {

    private const val LIBRARY_NAME = "data_store_demo"
    private const val NO_INSTANCE_ID = 0L
    private const val SAFE_AREA_CHANNEL = "istmo.safe_area"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()

    /**
     * Central native-handle ownership map. Every Kotlin dispatcher that
     * returns a `NativeHandleId` to Rust allocates it via [allocHandleId]
     * so [onReleaseNativeHandle] can route the release back to the
     * correct owner. The map is `handleId -> pluginId`.
     */
    private val handleOwners = ConcurrentHashMap<Long, String>()
    private val nextGlobalHandleId = AtomicLong(1)

    /** Pending Kotlin-initiated calls awaiting a `Frame::Respond`. */
    private val outboundCalls = ConcurrentHashMap<Long, PendingCall>()
    private val nextCallId = AtomicLong(1)

    init {
        System.loadLibrary(LIBRARY_NAME)
    }

    /** Register a Kotlin backend for a plugin id (native-hosted plugins). */
    fun registerHandler(pluginId: String, handler: PluginHandler) {
        handlers[pluginId] = handler
    }

    /**
     * Reserve a fresh `NativeHandleId` and record `pluginId` as the owner.
     *
     * Called by a Kotlin dispatcher every time it registers a new native
     * object it wants Rust to track (a credential, an AdMob ad, a
     * `Bitmap`, ...). [onReleaseNativeHandle] uses the recorded owner to
     * route the release back into the dispatcher's own registry.
     *
     * Global counter — the ids are unique across every dispatcher, which
     * makes the release routing deterministic (no collisions between
     * two dispatchers reusing local counters starting at 1).
     */
    fun allocHandleId(pluginId: String): Long {
        val id = nextGlobalHandleId.getAndIncrement()
        handleOwners[id] = pluginId
        return id
    }

    /**
     * Forget the ownership entry for `handleId`. Dispatchers call this
     * from their own release path (interstitial shown, credential freed)
     * to avoid a redundant [HandleReleaser.releaseNativeHandle] callback
     * when the eventual `Frame::ReleaseNativeHandle` arrives.
     */
    fun forgetHandle(handleId: Long) {
        handleOwners.remove(handleId)
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

    /**
     * Publish a `SafeAreaInsets` snapshot on the `istmo.safe_area`
     * early-event channel. Every f32 field is written in bincode's
     * standard fixed-width encoding — 4 IEEE-754 LE bytes each — so the
     * wire size is a constant 48 bytes (12 fields × 3 sub-rects).
     *
     * Field order matches the Rust `SafeAreaInsets` struct declaration:
     * `system_bars` (top, right, bottom, left), then `ime`, then
     * `display_cutout`. Drift here silently produces garbage insets in
     * Rust.
     */
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

    /**
     * Fire-and-forget counterpart of [onCall]: dispatches to the registered
     * handler and discards the outcome. No `Frame::Respond` is emitted; the
     * Rust runtime does not track a `call_id` for these.
     */
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
        // Instances are per-plugin; handlers own the teardown.
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
        // Kotlin-initiated streams aren't exercised by this demo.
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) {
        // See onEvent.
    }

    @JvmStatic
    fun onReleaseNativeHandle(handleId: Long) {
        // Look up the recorded owner and dispatch. Unknown ids are a
        // no-op — the release may race a same-thread dispatcher-side
        // release (interstitial shown then dropped), in which case the
        // owner entry has already been removed by `forgetHandle`.
        val ownerId = handleOwners.remove(handleId) ?: return
        (handlers[ownerId] as? HandleReleaser)?.releaseNativeHandle(handleId)
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
