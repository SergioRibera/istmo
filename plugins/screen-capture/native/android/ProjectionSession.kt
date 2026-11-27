package dev.istmo.plugins.screencapture

import android.hardware.display.DisplayManager
import android.hardware.display.VirtualDisplay
import android.os.Build
import android.os.Handler
import android.view.Display
import android.view.Surface
import dev.istmo.runtime.IstmoFrameSender
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.suspendCancellableCoroutine

/** Run [block] on this handler's thread and return its result. */
internal suspend fun <T> Handler.call(block: () -> T): T = suspendCancellableCoroutine { cont ->
    post {
        try {
            cont.resume(block())
        } catch (e: Throwable) {
            cont.resumeWithException(e)
        }
    }
}

/** Everything `open*` settled before the projection starts. */
internal class SessionPlan(
    val display: DisplayFacts,
    val mode: ProjectionMode,
    val size: OutputSize,
    /** Region in display pixels; `null` = the whole capture. */
    val region: Rect?,
    /** `null` for a preview-only session (no frames reach a sink). */
    val format: Negotiated?,
    val fps: FrameRate,
    val poolRequest: UByte,
    val exhaustion: Exhaustion,
    val audio: AudioPlan?,
    val indicator: IndicatorState,
)

/** Output size for content of [width] × [height] (even sizes: encoders and YUV consumers need them). */
internal fun outputSize(size: OutputSize, width: Int, height: Int): Pair<Int, Int> = when (size) {
    OutputSize.Native -> width to height
    is OutputSize.Scaled -> size.value.width.toInt() to size.value.height.toInt()
    is OutputSize.Fit -> {
        val scale = minOf(size.value.width.toDouble() / width, size.value.height.toDouble() / height)
        ((width * scale).toInt() and 1.inv()).coerceAtLeast(2) to ((height * scale).toInt() and 1.inv()).coerceAtLeast(2)
    }
}

/**
 * One projection session: a virtual display mirroring the default
 * display (or the app the person picked on API 34+) into a frame sink, a
 * preview surface, or both through [PreviewFanOut]; optionally playback
 * audio into a second sink. All state changes run on the backend's
 * capture thread ([handler]).
 */
internal class ProjectionSession(
    val id: Long,
    private val handler: Handler,
    private val displays: Displays,
    private val projection: Projection,
    private val plan: SessionPlan,
    private val video: IstmoFrameSender<FrameMeta>?,
    private val preview: Surface?,
    private val fit: PreviewFit,
    private val audioSender: IstmoFrameSender<AudioMeta>?,
    private val playback: (AudioPlan, IstmoFrameSender<AudioMeta>, (CaptureError?) -> Unit) -> PlaybackAudio,
    private val onEnded: (ProjectionSession) -> Unit,
) : ProjectionListener, DisplayManager.DisplayListener {
    private var source: Source = plan.display.source()
    private var contentWidth = plan.display.width
    private var contentHeight = plan.display.height
    private var outWidth = 0
    private var outHeight = 0
    private var region: Rect? = plan.region
    private var virtualDisplay: VirtualDisplay? = null
    private var stream: FrameStream? = null
    private var fanOut: PreviewFanOut? = null
    private var audio: PlaybackAudio? = null
    private var target: Surface? = null
    private var paused = false
    private var ended = false
    private val done = CompletableDeferred<Unit>()

    /** Called (capture thread) after each frame the sink took; screenshots wait on it. */
    @Volatile
    var onFirstFrame: (() -> Unit)? = null

    val hub: EventHub<CaptureEvent>

    init {
        val (w, h) = outputSize(plan.size, contentWidth, contentHeight)
        outWidth = w
        outHeight = h
        hub = EventHub(CaptureEvent.Started(negotiated()))
    }

    /** Create the virtual display and start delivering. Throws a `BackendException` on failure. */
    suspend fun start(): NegotiatedStream {
        projection.listener = this
        if (projection.isStopped) fail(CaptureError.ConsentRevoked)
        if (!projection.used.compareAndSet(false, true)) fail(CaptureError.GrantExpired)
        try {
            val format = plan.format
            val sink = video
            val readerSurface = if (format != null && sink != null) {
                handler.call {
                    val created = FrameStream(
                        handler, sink, format.offer, FrameOrigin(source.id, plan.display.scalePermille),
                        FrameStream.poolFor(plan.poolRequest, plan.fps), plan.exhaustion, plan.fps,
                        events = hub::publish,
                        onSinkGone = { end(StopReason.Closed, null) },
                        onFailed = { reason -> handler.post { end(StopReason.Failed, reason) } },
                    )
                    created.configure(format.offer, format.tier, created.poolDepth)
                    created.onDelivered = { onFirstFrame?.invoke() }
                    stream = created
                    val surface = created.open(outWidth, outHeight)
                    created.contentRect = contentRect()
                    surface
                }
            } else {
                null
            }
            val renderInto = when {
                preview != null && readerSurface != null -> {
                    val gl = PreviewFanOut(preview, fit) { reason -> handler.post { end(StopReason.Failed, reason) } }
                    fanOut = gl
                    gl.start(outWidth, outHeight, readerSurface)
                }
                preview != null -> preview
                readerSurface != null -> readerSurface
                else -> fail(CaptureError.Protocol(BackendDetail(PROJECTION, "open", "nothing to render into")))
            }
            target = renderInto
            handler.call {
                virtualDisplay = projection.projection.createVirtualDisplay(
                    VIRTUAL_DISPLAY_NAME, outWidth, outHeight, plan.display.densityDpi,
                    DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, renderInto, null, handler,
                ) ?: fail(CaptureError.Protocol(BackendDetail(PROJECTION, "createVirtualDisplay", "returned null")))
                // API 34+ reports size changes (rotation included) through
                // `onCapturedContentResize`; older releases only through
                // the display itself.
                if (Build.VERSION.SDK_INT < Api.U) displays.manager.registerDisplayListener(this, handler)
            }
            val audioPlan = plan.audio
            if (audioPlan != null && audioSender != null) {
                val capture = playback(audioPlan, audioSender) { error ->
                    if (error != null) audioSender.closeWithError(error.toString()) else audioSender.close()
                }
                capture.start()
                audio = capture
            }
        } catch (e: Throwable) {
            handler.post { end(StopReason.Failed, e.toString()) }
            if (e is kotlinx.coroutines.CancellationException) throw e
            throw when (e) {
                is dev.istmo.runtime.BackendException -> e
                is SecurityException -> dev.istmo.runtime.BackendException(CaptureError.ConsentRevoked)
                else -> dev.istmo.runtime.BackendException(
                    CaptureError.Protocol(BackendDetail(PROJECTION, "open", e.toString())),
                )
            }
        }
        val negotiated = negotiated()
        hub.started = CaptureEvent.Started(negotiated)
        return negotiated
    }

    /** The crop the caller asked for, mapped into the output and clipped to it. */
    private fun contentRect(): Rect {
        val asked = region ?: return Rect(0, 0, outWidth.toUInt(), outHeight.toUInt())
        val sx = outWidth.toDouble() / contentWidth
        val sy = outHeight.toDouble() / contentHeight
        val left = (asked.x * sx).toInt().coerceIn(0, outWidth)
        val top = (asked.y * sy).toInt().coerceIn(0, outHeight)
        val right = ((asked.x + asked.width.toInt()) * sx).toInt().coerceIn(left, outWidth)
        val bottom = ((asked.y + asked.height.toInt()) * sy).toInt().coerceIn(top, outHeight)
        return Rect(left, top, (right - left).toUInt(), (bottom - top).toUInt())
    }

    fun negotiated(): NegotiatedStream {
        val format = plan.format
        val live = stream
        val copies = buildList {
            if (format != null) addAll(format.offer.copies)
            if (preview != null && format != null) add(CopyStage.PreviewFanOut)
        }
        return NegotiatedStream(
            backend = PROJECTION,
            source = source,
            size = Size(outWidth.toUInt(), outHeight.toUInt()),
            contentRect = contentRect(),
            format = live?.offer?.format ?: format?.offer?.format ?: PixelFormat.Rgba8,
            memory = live?.offer?.memory ?: format?.offer?.memory ?: BufferMemory.HardwareBuffer,
            modifier = null,
            tier = (live?.tier ?: format?.tier ?: 0).toUByte(),
            copies = copies,
            fps = live?.fps ?: if (format == null) FrameRate.Native else plan.fps,
            sourceRateMhz = (plan.display.refreshHz * 1000f).toUInt(),
            producer = null,
            sync = SyncKind.Ready,
            poolDepth = (live?.poolDepth ?: 0).toUByte(),
            exhaustion = plan.exhaustion,
            orientation = OrientationSupport.AlreadyApplied,
            crop = RegionCrop.InMeta,
            damage = DamageSupport.Frame,
            cursor = CursorMode.Hidden,
            colour = SRGB,
            hdr = false,
            composite = null,
            consent = ConsentModel.PerSessionDialog,
            indicator = plan.indicator,
            border = BorderState.NotApplicable,
            restoreToken = null,
            restored = false,
            audio = plan.audio?.negotiated,
        )
    }

    // ---- Projection and display callbacks (capture thread) -----------

    override fun onProjectionStop() {
        handler.post { end(StopReason.ConsentRevoked, "screen capture consent revoked") }
    }

    override fun onContentResize(width: Int, height: Int) {
        handler.post {
            if (ended || width <= 0 || height <= 0) return@post
            // Android does not say whether the person shared the display
            // or one app: a captured region of another size than the
            // display is an app window.
            val display = runCatching { displays.primary() }.getOrNull()
            if (plan.mode != ProjectionMode.EntireScreen && display != null &&
                source.kind == SourceKind.Monitor && (width != display.width || height != display.height)
            ) {
                source = source.copy(
                    id = SourceId(PROJECTION, SHARED_APP_ID),
                    kind = SourceKind.Application,
                    name = "Shared app",
                    capture = SourceCapture.PickerOnly(listOf(PROJECTION)),
                )
                hub.publish(CaptureEvent.Renegotiated(negotiated()))
            }
            resize(width, height)
        }
    }

    override fun onContentVisible(visible: Boolean) {
        handler.post {
            if (!ended) hub.publish(if (visible) CaptureEvent.SourceVisible else CaptureEvent.SourceHidden)
        }
    }

    override fun onDisplayAdded(displayId: Int) {}

    override fun onDisplayRemoved(displayId: Int) {}

    override fun onDisplayChanged(displayId: Int) {
        if (displayId != Display.DEFAULT_DISPLAY || ended) return
        val facts = runCatching { displays.primary() }.getOrNull() ?: return
        resize(facts.width, facts.height)
    }

    /** The captured content is now [width] × [height]: new virtual display size and pool. */
    private fun resize(width: Int, height: Int) {
        if (width == contentWidth && height == contentHeight) return
        contentWidth = width
        contentHeight = height
        val (w, h) = outputSize(plan.size, width, height)
        if (w == outWidth && h == outHeight) return
        outWidth = w
        outHeight = h
        val display = virtualDisplay ?: return
        val live = stream
        val readerSurface = live?.open(w, h)
        live?.contentRect = contentRect()
        val gl = fanOut
        if (gl != null && readerSurface != null) {
            // Queued on the GL thread ahead of the next frame callback.
            gl.resizeLater(w, h, readerSurface)
        } else if (readerSurface != null && !paused) {
            display.surface = readerSurface
            target = readerSurface
        } else if (readerSurface != null) {
            target = readerSurface
        }
        display.resize(w, h, plan.display.densityDpi)
        hub.publish(CaptureEvent.SourceResized(Size(w.toUInt(), h.toUInt())))
    }

    // ---- Control (any thread; runs on the capture thread) -----------

    suspend fun update(update: SessionUpdate): NegotiatedStream = handler.call {
        if (ended) fail(CaptureError.SessionNotFound)
        val live = stream
        when (update) {
            is SessionUpdate.Fps -> {
                if (live == null) fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Operation))))
                val pool = FrameStream.poolFor(plan.poolRequest, update.value)
                live.updateFps(update.value)
                if (pool > live.poolDepth) reopen(live, live.offer, live.tier, pool)
            }
            is SessionUpdate.Cursor ->
                if (update.value.strength == Strength.Require && update.value.mode != CursorMode.Hidden) {
                    fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Cursor))))
                }
            is SessionUpdate.Region -> {
                region = regionPixels(plan.display, update.value)
                    ?: fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Region))))
                live?.contentRect = contentRect()
            }
            is SessionUpdate.Formats -> {
                if (live == null) fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Operation))))
                val next = negotiate(update.value, cpuFirst = false, plan.display)
                    ?: fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Formats))))
                if (next.offer != live.offer || next.tier != live.tier) reopen(live, next.offer, next.tier, live.poolDepth)
            }
            SessionUpdate.Pause -> if (!paused) {
                paused = true
                live?.paused = true
                virtualDisplay?.surface = null
                hub.publish(CaptureEvent.Paused(PauseReason.Requested))
            }
            SessionUpdate.Resume -> if (paused) {
                paused = false
                live?.paused = false
                virtualDisplay?.surface = target
                hub.publish(CaptureEvent.Resumed)
            }
        }
        val negotiated = negotiated()
        if (update !is SessionUpdate.Pause && update !is SessionUpdate.Resume && update !is SessionUpdate.Cursor) {
            hub.publish(CaptureEvent.Renegotiated(negotiated))
        }
        negotiated
    }

    /** A new pool (new generation) with another offer, tier or depth. Capture thread. */
    private fun reopen(live: FrameStream, offer: Offer, tier: Int, pool: Int) {
        live.configure(offer, tier, pool)
        val surface = live.open(outWidth, outHeight)
        live.contentRect = contentRect()
        val gl = fanOut
        if (gl != null) {
            gl.resizeLater(outWidth, outHeight, surface)
        } else {
            target = surface
            if (!paused) virtualDisplay?.surface = surface
        }
    }

    fun setAudioPaused(value: Boolean) {
        audio?.paused = value
    }

    /** Close and wait until the session is torn down. */
    suspend fun close() {
        handler.post { end(StopReason.Closed, null) }
        done.await()
    }

    /**
     * Tear down (idempotent, capture thread): virtual display, projection,
     * audio, the sinks (closed with [error] as their end reason, after
     * what they hold), then `Stopped`.
     */
    fun end(reason: StopReason, error: String?) {
        if (ended) return
        ended = true
        projection.listener = null
        if (Build.VERSION.SDK_INT < Api.U) runCatching { displays.manager.unregisterDisplayListener(this) }
        virtualDisplay?.release()
        virtualDisplay = null
        audio?.stop()
        projection.stop()
        stream?.close()
        fanOut?.release()
        if (error == null) {
            video?.close()
            audioSender?.close()
        } else {
            video?.closeWithError(error)
            audioSender?.closeWithError(error)
        }
        if (reason == StopReason.ConsentRevoked) hub.publish(CaptureEvent.ConsentRevoked)
        hub.publish(CaptureEvent.Stopped(reason))
        hub.close()
        onEnded(this)
        done.complete(Unit)
    }

    companion object {
        const val VIRTUAL_DISPLAY_NAME = "istmo-screen-capture"
        const val SHARED_APP_ID = "app:shared"
    }
}

/** [selector] in display pixels, clipped to the display; `null` when it names another display or misses it. */
internal fun regionPixels(display: DisplayFacts, selector: RegionSelector): Rect? {
    when (val space = selector.space) {
        RegionSpace.Global -> {}
        is RegionSpace.MonitorRelative -> if (space.value != display.sourceId) return null
    }
    val left = display.toPixels(selector.rect.x).coerceIn(0, display.width)
    val top = display.toPixels(selector.rect.y).coerceIn(0, display.height)
    val right = display.toPixels(selector.rect.x + selector.rect.width.toInt()).coerceIn(left, display.width)
    val bottom = display.toPixels(selector.rect.y + selector.rect.height.toInt()).coerceIn(top, display.height)
    if (right <= left || bottom <= top) return null
    return Rect(left, top, (right - left).toUInt(), (bottom - top).toUInt())
}
