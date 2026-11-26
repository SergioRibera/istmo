package dev.istmo.runtime

import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.CancellableContinuation
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.CoroutineStart
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.isActive
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.channels.SendChannel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine

object IstmoRuntime {

    private const val NO_INSTANCE_ID = 0L
    private const val SAFE_AREA_CHANNEL = "istmo.safe_area"

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val jobs = ConcurrentHashMap<Long, Job>()
    private val handlers = ConcurrentHashMap<String, PluginHandler>()
    private val services = ConcurrentHashMap<String, android.app.Service>()

    private val handleOwners = ConcurrentHashMap<Long, String>()
    private val appObjects = ConcurrentHashMap<Long, Any>()
    private val nextGlobalHandleId = AtomicLong(1)

    private val outboundCalls = ConcurrentHashMap<Long, PendingCall>()
    private val outboundStreams = ConcurrentHashMap<Long, PendingStream>()
    private val nextCallId = AtomicLong(1)

    /** Early events published before the native library was loaded. */
    private val pendingEarlyQueue = java.util.ArrayDeque<Triple<String, Int, ByteArray>>()

    /** Plugins whose calls always start on `Dispatchers.Default`. */
    private val dispatchedHandlers = ConcurrentHashMap.newKeySet<String>()

    /**
     * Serve calls to [pluginId] with [handler].
     *
     * A unary call Rust makes from one of its own threads starts on that
     * thread by default and runs there until its first suspension: a
     * backend that never suspends answers before the Rust caller's call
     * returns, one that suspends continues wherever it resumes (its own
     * dispatcher, or `Dispatchers.Default`). The Rust thread can be any
     * thread, the main thread included when a Kotlin call on main reached
     * a Rust host that calls back. Everything else (calls, streams,
     * notifications and instance creation the runtime's delivery thread
     * hands over) starts on `Dispatchers.Default`, never on that thread.
     *
     * Like any `suspend fun`, a backend method must not block the thread:
     * wrap blocking work (disk, binder calls that can stall, content
     * providers) in `withContext(Dispatchers.IO)`. `withContext(Dispatchers.Default)`
     * does not move work off a Rust thread started in place only when
     * that thread already is a `Default` worker; pass [startInPlace]
     * `false` to start every call on `Dispatchers.Default`.
     */
    @JvmOverloads
    fun registerHandler(pluginId: String, handler: PluginHandler, startInPlace: Boolean = true) {
        if (startInPlace) dispatchedHandlers.remove(pluginId) else dispatchedHandlers.add(pluginId)
        handlers[pluginId] = handler
    }

    /**
     * Drop the handler registered for [pluginId]; later calls to it fail
     * as for an unregistered plugin. Calls already running keep their
     * handler until they finish.
     */
    fun unregisterHandler(pluginId: String) {
        handlers.remove(pluginId)
        dispatchedHandlers.remove(pluginId)
    }

    /**
     * Expose a running [android.app.Service] under [pluginId] so backends
     * (foreground notification, wake locks, `stopSelf`) can reach it by
     * the service id Rust passes them. Generated `#[istmo::service]`
     * wrappers call this from `onCreate`.
     */
    fun registerService(pluginId: String, service: android.app.Service) {
        services[pluginId] = service
    }

    /** Drop the service registered for [pluginId]; called from `onDestroy`. */
    fun unregisterService(pluginId: String) {
        services.remove(pluginId)
    }

    /** The running service registered for [pluginId], if any. */
    fun service(pluginId: String): android.app.Service? = services[pluginId]

    fun allocHandleId(pluginId: String): Long {
        val id = nextGlobalHandleId.getAndIncrement()
        handleOwners[id] = pluginId
        return id
    }

    fun forgetHandle(handleId: Long) {
        handleOwners.remove(handleId)
    }

    /**
     * Register a platform object app code hands to a plugin (a preview
     * `Surface`, a consent grant) and get the `NativeHandleId` to pass as
     * the plugin argument. The plugin's backend reads it back with
     * [objectFor]; it is released when Rust drops its `NativeHandle`, or
     * with [releaseObject].
     */
    fun registerObject(obj: Any): Long {
        val id = nextGlobalHandleId.getAndIncrement()
        appObjects[id] = obj
        return id
    }

    /** The object [registerObject] stored under [handleId], if it is a [T]. */
    inline fun <reified T : Any> objectFor(handleId: Long): T? = objectAny(handleId) as? T

    /** The object stored under [handleId], untyped. */
    fun objectAny(handleId: Long): Any? = appObjects[handleId]

    /** Forget an object [registerObject] stored. */
    fun releaseObject(handleId: Long) {
        appObjects.remove(handleId)
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
        IstmoFrames.checkAbi()
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

    /**
     * Publish [payload] as the current value of the early-event slot
     * [channel] (see `istmo_core::early_events::LatestValueSlot`). Needs
     * the native library loaded; the Rust side holds the value until the
     * runtime is initialised.
     */
    fun publishEarlyLatest(channel: String, payload: ByteArray) {
        nativeSubmitEarlyLatest(channel, payload)
    }

    private fun flushPendingEarlyQueueLocked() {
        while (pendingEarlyQueue.isNotEmpty()) {
            val (channel, capacity, payload) = pendingEarlyQueue.peekFirst() ?: return
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
            pending.complete(Result.failure(kotlinx.coroutines.CancellationException("runtime shut down")))
        }
        outboundCalls.clear()
        for ((_, pending) in outboundStreams) {
            pending.channel.close(StreamCancelledException("runtime shut down"))
        }
        outboundStreams.clear()
    }

    /**
     * Call a Rust-hosted plugin with the first [payloadLen] bytes of
     * [payload]. A host that answers without waiting answers right here,
     * on the calling thread, with no suspension; otherwise the call
     * suspends until the answer arrives. Errors throw [PluginException].
     */
    @JvmOverloads
    suspend fun call(
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
        payloadLen: Int = payload.size,
    ): ByteArray {
        // A cancelled caller gets its CancellationException, whether or
        // not the host would have answered in place.
        currentCoroutineContext().ensureActive()
        val callId = nextCallId.getAndIncrement()
        // Registered first: an answer that comes through `onRespond`
        // before this coroutine suspends waits in the entry.
        val pending = PendingCall()
        outboundCalls[callId] = pending
        val direct = try {
            nativeCall(callId, pluginId, instanceId, method, payload, payloadLen)
        } catch (e: Throwable) {
            // A thrown `PluginException` is the host's error answer; any
            // other throwable (a pending JNI error, UnsatisfiedLinkError)
            // must not leak the entry either.
            outboundCalls.remove(callId)
            throw e
        }
        if (direct != null) {
            outboundCalls.remove(callId)
            // Cancelled while the host ran in place: the answer is dropped.
            currentCoroutineContext().ensureActive()
            return direct
        }
        return suspendCancellableCoroutine { cont ->
            cont.invokeOnCancellation {
                // Cancellation handlers must not throw; a cancel racing
                // shutdown has nothing left to stop anyway.
                if (outboundCalls.remove(callId) != null) {
                    runCatching { nativeSubmitCancel(callId) }
                }
            }
            pending.attach(cont)
        }
    }

    suspend fun call(pluginId: String, method: String, payload: ByteArray): ByteArray =
        call(pluginId, NO_INSTANCE_ID, method, payload)

    /**
     * Open a stream on a Rust-hosted `#[istmo::stream]` method. Cold: the
     * call goes out when the flow is collected, once per collection.
     *
     * The flow completes when the host closes the stream, fails with
     * [PluginException] carrying the encoded domain error when the host
     * ends it with an error (or rejects the call), and with
     * [StreamCancelledException] when the host cancels it. Cancelling the
     * collector cancels the call on the Rust side.
     *
     * Backpressure: the host sends at most [bufferCapacity] items ahead
     * of the collector. Each item the collector takes (its `emit`
     * returned) is credited back to Rust in batches of half the window,
     * and a Rust producer that runs ahead waits — its stream's bounded
     * channel fills — instead of overflowing this buffer. Nothing is
     * dropped. A collector that only wants the latest value applies
     * `.conflate()` downstream.
     */
    fun stream(
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
        bufferCapacity: Int = DEFAULT_STREAM_BUFFER,
    ): Flow<ByteArray> {
        require(bufferCapacity > 0) { "bufferCapacity must be positive" }
        return flow {
            val callId = nextCallId.getAndIncrement()
            // Rust never sends more than the credit it holds, which is at
            // most this capacity: `onEvent`'s `trySend` always has room.
            val items = Channel<ByteArray>(bufferCapacity)
            outboundStreams[callId] = PendingStream(items)
            try {
                nativeSubmitStream(callId, pluginId, instanceId, method, payload, payload.size, bufferCapacity)
            } catch (e: Throwable) {
                outboundStreams.remove(callId)
                throw e
            }
            val grantEvery = maxOf(1, bufferCapacity / 2)
            var taken = 0
            try {
                for (item in items) {
                    emit(item)
                    taken++
                    if (taken >= grantEvery) {
                        nativeStreamCredit(callId, taken)
                        taken = 0
                    }
                }
            } finally {
                // Still registered means the host has not ended it: the
                // collector left early, so stop the producer.
                if (outboundStreams.remove(callId) != null) {
                    runCatching { nativeSubmitCancel(callId) }
                }
            }
        }
    }

    fun stream(
        pluginId: String,
        method: String,
        payload: ByteArray,
        bufferCapacity: Int = DEFAULT_STREAM_BUFFER,
    ): Flow<ByteArray> = stream(pluginId, NO_INSTANCE_ID, method, payload, bufferCapacity)

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
        // Delivered by the pump: never run a backend on its thread.
        launchHosted(callId, inPlace = false) {
            when (val result = handler.handleCall(instanceId, method, payload)) {
                is PluginResult.Unary -> nativeSubmitResponse(callId, true, result.bytes)
                is PluginResult.Stream -> pumpStream(callId, result.flow)
            }
        }
    }

    /**
     * A call from Rust delivered on the Rust caller's own thread: runs
     * like [onCall], and returns the reply bytes when the backend
     * answered without suspending. `null`: the answer (or an error) goes
     * through `nativeSubmitResponse`, possibly before this returns.
     */
    @JvmStatic
    fun onCallDirect(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray? {
        val handler = handlers[pluginId]
        if (handler == null) {
            nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            return null
        }
        val direct = DirectReply()
        launchHosted(callId, inPlace = pluginId !in dispatchedHandlers) {
            when (val result = handler.handleCall(instanceId, method, payload)) {
                is PluginResult.Unary ->
                    if (!direct.offer(result.bytes)) nativeSubmitResponse(callId, true, result.bytes)
                is PluginResult.Stream -> pumpStream(callId, result.flow)
            }
        }
        return direct.close()
    }

    /**
     * A direct call's answer while [onCallDirect] has not returned: taken
     * as its return value, or sent once the call has left.
     */
    private class DirectReply {
        private var waiting = true
        private var bytes: ByteArray? = null

        @Synchronized
        fun offer(answer: ByteArray): Boolean {
            if (!waiting) return false
            bytes = answer
            waiting = false
            return true
        }

        @Synchronized
        fun close(): ByteArray? {
            waiting = false
            return bytes.also { bytes = null }
        }
    }

    /**
     * Run a hosted call as a coroutine registered under [callId] (so
     * `onCancel` reaches it): on the calling thread until its first
     * suspension when [inPlace], on `Dispatchers.Default` otherwise. A
     * [PluginException] answers with its payload, anything else with an
     * empty error; only the call's own cancellation answers nothing.
     */
    private inline fun launchHosted(callId: Long, inPlace: Boolean, crossinline body: suspend () -> Unit) {
        // The job exists before the body runs: a body that finishes in
        // place, or a cancel that arrives while it runs, finds it.
        val job = Job(scope.coroutineContext[Job])
        jobs[callId] = job
        val start = if (inPlace) CoroutineStart.UNDISPATCHED else CoroutineStart.DEFAULT
        scope.launch(job, start) {
            try {
                body()
            } catch (e: PluginException) {
                nativeSubmitResponse(callId, false, e.payload)
            } catch (e: kotlinx.coroutines.CancellationException) {
                // Rust cancelled the call (or the runtime shut down):
                // nobody waits for an answer. Any other cancellation (an
                // uncaught `withTimeout`, a cancelled task awaited) is the
                // backend failing, and Rust still waits for its answer.
                if (!isActive) throw e
                nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            } catch (_: Throwable) {
                nativeSubmitResponse(callId, false, EMPTY_PAYLOAD)
            } finally {
                jobs.remove(callId, job)
                job.complete()
            }
        }
    }

    private suspend fun pumpStream(callId: Long, flow: kotlinx.coroutines.flow.Flow<ByteArray>) {
        try {
            // A stream opened in place runs on the Rust caller's thread:
            // collect it on `Default` instead (a no-op hop when already
            // there), since a flow may emit for as long as it likes.
            kotlinx.coroutines.withContext(Dispatchers.Default) {
                // `Unconfined` keeps the collector on the thread that
                // emitted the item instead of hopping onto a `Default`
                // worker per sample: for high-rate streams (stylus input,
                // pen hover) the per-event hop added tens of ms of visible
                // latency. Item by item, on the emitting thread: a backend
                // that emits without suspending (a sensor callback, a busy
                // loop) gets every item across at once; Rust holds it back
                // when its consumer falls behind.
                kotlinx.coroutines.withContext(Dispatchers.Unconfined) {
                    flow.collect { bytes ->
                        // Holding an item back blocks the emitting thread,
                        // which must never be the delivery thread (an
                        // upstream fed by a Rust stream resumes there).
                        if (onDeliveryThread()) {
                            kotlinx.coroutines.withContext(Dispatchers.Default) { nativeSubmitEvent(callId, bytes) }
                        } else {
                            nativeSubmitEvent(callId, bytes)
                        }
                    }
                }
            }
            nativeSubmitStreamEnd(callId, STREAM_END_COMPLETE, null)
        } catch (e: kotlinx.coroutines.CancellationException) {
            nativeSubmitStreamEnd(callId, STREAM_END_CANCELLED, null)
            // Only the call's own cancellation goes on up; any other ends
            // the stream like a failure.
            if (!currentCoroutineContext().isActive) throw e
        } catch (e: PluginException) {
            // Generated dispatchers turn a domain error into its encoded bytes.
            nativeSubmitStreamEnd(callId, STREAM_END_ERROR, e.payload)
        } catch (_: Throwable) {
            nativeSubmitStreamEnd(callId, STREAM_END_CANCELLED, null)
        }
    }

    /** The runtime's delivery thread (Rust's pump), once seen. */
    @Volatile
    private var deliveryThread: Thread? = null

    private fun onDeliveryThread(): Boolean {
        val current = Thread.currentThread()
        deliveryThread?.let { return it === current }
        if (current.name != DELIVERY_THREAD_NAME) return false
        deliveryThread = current
        return true
    }

    /** Name of the Rust pump thread (`crates/android/src/pump.rs`). */
    private const val DELIVERY_THREAD_NAME = "istmo-android-pump"

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
        // Delivered by the pump: never run a backend on its thread.
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
        launchHosted(callId, inPlace = false) {
            nativeSubmitResponse(callId, true, handler.handleCreateInstance(payload))
        }
    }

    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun onDestroyInstance(instanceId: Long) {

    }

    @JvmStatic
    fun onRespond(callId: Long, ok: Boolean, payload: ByteArray) {
        val pending = outboundCalls.remove(callId)
        if (pending == null) {
            // A stream call answered with a response instead of events:
            // the host rejected it (unknown method, undecodable payload)
            // or the method is unary.
            val stream = outboundStreams.remove(callId) ?: return
            stream.channel.close(
                if (ok) IllegalStateException("unary response to a stream call") else PluginException(payload),
            )
            return
        }
        pending.complete(if (ok) Result.success(payload) else Result.failure(PluginException(payload)))
    }

    @JvmStatic
    fun onEvent(streamId: Long, payload: ByteArray) {
        val stream = outboundStreams[streamId] ?: return
        offer(streamId, stream, payload)
    }

    /** Several items of one stream, in order. */
    @JvmStatic
    fun onEvents(streamId: Long, payloads: Array<ByteArray>) {
        val stream = outboundStreams[streamId] ?: return
        for (payload in payloads) {
            if (!offer(streamId, stream, payload)) return
        }
    }

    /** Queue one item; `false` once the stream is gone. */
    private fun offer(streamId: Long, stream: PendingStream, payload: ByteArray): Boolean {
        val sent = stream.channel.trySend(payload)
        if (sent.isSuccess) return true
        // A closed channel means the collector already left and cancels
        // the call itself; a full buffer (credit overrun, or a stream
        // opened without credit) is overflow.
        if (!sent.isClosed && outboundStreams.remove(streamId) != null) {
            stream.channel.close(StreamOverflowException())
            runCatching { nativeSubmitCancel(streamId) }
        }
        return false
    }

    @JvmStatic
    fun onStreamEnd(streamId: Long, reason: Int, errorPayload: ByteArray) {
        val stream = outboundStreams.remove(streamId) ?: return
        // Items already buffered stay receivable ahead of the close.
        when (reason) {
            STREAM_END_COMPLETE -> stream.channel.close()
            STREAM_END_ERROR -> stream.channel.close(PluginException(errorPayload))
            else -> stream.channel.close(StreamCancelledException("stream cancelled by its host"))
        }
    }

    @JvmStatic
    fun onReleaseNativeHandle(handleId: Long) {
        if (appObjects.remove(handleId) != null) return
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
    external fun nativeCall(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
        payloadLen: Int,
    ): ByteArray?
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
    external fun nativeSubmitStream(
        callId: Long,
        pluginId: String,
        instanceId: Long,
        method: String,
        payload: ByteArray,
        payloadLen: Int,
        window: Int,
    )
    external fun nativeStreamCredit(streamId: Long, items: Int)
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

    private const val DEFAULT_STREAM_BUFFER = 256

    /**
     * An outbound call's answer slot: whichever of the answer and the
     * suspended caller arrives second resumes the caller.
     */
    private class PendingCall {
        private var cont: CancellableContinuation<ByteArray>? = null
        private var outcome: Result<ByteArray>? = null

        fun complete(result: Result<ByteArray>) {
            val waiting = synchronized(this) {
                cont ?: run {
                    outcome = result
                    null
                }
            }
            waiting?.resumeWith(result)
        }

        fun attach(waiting: CancellableContinuation<ByteArray>) {
            val ready = synchronized(this) {
                outcome ?: run {
                    cont = waiting
                    null
                }
            }
            if (ready != null) waiting.resumeWith(ready)
        }
    }

    private class PendingStream(val channel: SendChannel<ByteArray>)
}

