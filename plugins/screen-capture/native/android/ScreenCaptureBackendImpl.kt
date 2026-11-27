package dev.istmo.plugins.screencapture

import android.Manifest
import android.app.Activity
import android.app.NotificationManager
import android.content.Context
import android.content.pm.PackageManager
import android.hardware.display.DisplayManager
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.view.Surface
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import dev.istmo.media.AccessStatus
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoActivityBound
import dev.istmo.runtime.IstmoFrameSender
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NativeHandleId
import java.lang.ref.WeakReference
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.resume
import kotlin.math.roundToInt
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.channels.awaitClose
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.callbackFlow
import kotlinx.coroutines.flow.emitAll
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/**
 * MediaProjection backend (`android-projection`) of `istmo.screen_capture`.
 *
 * - Consent: `createScreenCaptureIntent` (with `MediaProjectionConfig` on
 *   API 34+: the default display, or the person's choice of display or
 *   app) through the host activity's `ActivityResultRegistry`, inside
 *   `open*` / `screenshot*` when `allowPicker` is set, or ahead of time
 *   with `request_projection` (a single-use grant handle, dead after
 *   60 s or the projection's `onStop`). The projection runs under
 *   [IstmoProjectionService] (foreground service type `mediaProjection`),
 *   started after the consent and before `getMediaProjection`.
 * - Frames: a virtual display mirroring the default display into an
 *   `ImageReader`: `RGBA_8888` `HardwareBuffer`s with GPU usage handed to
 *   Rust without a copy (API 29+), or CPU-readable images copied into the
 *   sink (`CopyStage.GpuReadback`). Timestamps are the images'
 *   `CLOCK_MONOTONIC` ones; `wallUs` is sampled per frame.
 * - Preview: the virtual display renders straight into an app `Surface`
 *   (`IstmoRuntime.registerObject(surface)`); with frames as well, the
 *   GPU fan-out draws each frame into both (`CopyStage.PreviewFanOut`).
 * - Audio: playback capture (API 29+, `RECORD_AUDIO`), with or without
 *   video; audio-only sessions take a grant from `request_projection`.
 *
 * Built once per process ([IstmoActivityBound]): grants and sessions
 * survive the host activity's recreation. The Kotlin host sees no call
 * origin; grants and sessions use random or runtime-allocated ids that
 * only this process resolves.
 */
class ScreenCaptureBackendImpl(activity: ComponentActivity) : ScreenCaptureBackend, IstmoActivityBound, HandleReleaser {
    private val appContext: Context = activity.applicationContext

    @Volatile
    private var activityRef = WeakReference(activity)
    private val captureThread = HandlerThread("istmo-screen-capture").apply { start() }
    private val handler = Handler(captureThread.looper)
    private val displays = Displays(appContext)
    private val consent = ProjectionConsent(appContext, handler).also { it.attach(activity) }
    private val grants = ProjectionGrants(handler)
    private val sessions = ConcurrentHashMap<Long, ProjectionSession>()
    private val audioSessions = ConcurrentHashMap<Long, AudioSession>()
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)
    private val permissionPrefs by lazy {
        appContext.getSharedPreferences("${appContext.packageName}.istmo.screen_capture", Context.MODE_PRIVATE)
    }

    init {
        ProjectionForeground.onStopRequested = ::stopFromNotification
    }

    override fun attachActivity(activity: Activity) {
        // The registry only hands over activities of the declared class.
        val host = activity as ComponentActivity
        activityRef = WeakReference(host)
        consent.attach(host)
    }

    // ---- Reports ------------------------------------------------------

    override suspend fun backends(): List<BackendStatus> =
        listOf(BackendStatus(PROJECTION, if (consent.available) Availability.Ready else consent.unavailable()))

    override suspend fun capabilities(query: CapabilityQuery): CapabilityReport {
        query.backend?.let { if (it != PROJECTION) fail(CaptureError.BackendNotCompiled(it)) }
        query.source?.let { id ->
            val known = id.backend == PROJECTION &&
                (id.value == ProjectionSession.SHARED_APP_ID || displays.all().any { it.sourceId == id })
            if (!known) fail(CaptureError.SourceNotFound(id))
        }
        return CapabilityReport(listOf(caps()), AudioReport(audioAvailability(), audioCaps()))
    }

    private fun caps(): BackendCaps {
        val display = runCatching { displays.primary() }.getOrNull()
        return BackendCaps(
            backend = PROJECTION,
            sourceKinds = if (Build.VERSION.SDK_INT >= Api.U) {
                listOf(SourceKind.Monitor, SourceKind.Application)
            } else {
                listOf(SourceKind.Monitor)
            },
            listing = SourceListing.Enumerable,
            consent = ConsentModel.PerSessionDialog,
            video = Offer.available().map { FormatCaps(it.format, it.memory, emptyList(), it.copies) },
            maxFps = display?.refreshHz?.roundToInt()?.toUInt(),
            minIntervalUs = null,
            maxSize = display?.let { Size(maxOf(it.width, it.height).toUInt(), maxOf(it.width, it.height).toUInt()) },
            scaling = Scaling.Native,
            region = RegionCrop.InMeta,
            composite = CompositeCaps(CompositeSupport.Unsupported, CompositeSupport.Unsupported),
            // The pointer sprite is not composed into virtual displays.
            cursor = listOf(CursorMode.Hidden),
            damage = DamageSupport.Frame,
            orientation = OrientationSupport.AlreadyApplied,
            colour = ColourCaps(listOf(SRGB), false),
            audio = audioCaps(),
            producer = null,
            sync = listOf(SyncKind.Ready),
            persistence = Persistence.Unsupported,
            // A new projection stops the previous one.
            concurrency = 1u,
            pool = PoolCaps(
                FrameStream.MIN_POOL.toUByte(), FrameStream.MAX_POOL.toUByte(), FrameStream.DEFAULT_POOL.toUByte(),
                listOf(Exhaustion.Stall, Exhaustion.DropNewest),
            ),
            copies = listOf(CopyStage.GpuReadback, CopyStage.PreviewFanOut),
            protectedContent = ProtectedContent.MaskedUndetectable,
            window = WindowCaptureCaps(occluded = false, minimised = false, childPopups = false, border = false),
            provenance = CapsProvenance(
                sourceKinds = Provenance.Table,
                listing = Provenance.Probe,
                video = Provenance.Table,
                cursor = Provenance.Table,
                damage = Provenance.Table,
                colour = Provenance.Table,
                audio = Provenance.Table,
                producer = Provenance.Table,
                sync = Provenance.Table,
                persistence = Provenance.Table,
            ),
            notes = emptyList(),
        )
    }

    private fun audioCaps(): AudioCaps =
        if (Build.VERSION.SDK_INT >= Api.Q) {
            AudioCaps(
                listOf(AudioSourceKind.System, AudioSourceKind.Application, AudioSourceKind.ExcludeSelf),
                listOf(SampleFormat.S16, SampleFormat.F32),
                mayBeSilent = true,
            )
        } else {
            AudioCaps(emptyList(), emptyList(), mayBeSilent = true)
        }

    private fun audioAvailability(): Availability = when {
        Build.VERSION.SDK_INT < Api.Q -> Availability.OsTooOld(OsVersion(10u, 0u, 0u))
        !granted(Manifest.permission.RECORD_AUDIO) -> Availability.NeedsAccess(AccessScope.Microphone)
        else -> Availability.Ready
    }

    // ---- Sources ------------------------------------------------------

    private fun listSources(filter: SourceFilter): List<Source> {
        filter.backend?.let { if (it != PROJECTION) fail(CaptureError.BackendNotCompiled(it)) }
        if (filter.kinds.isNotEmpty() && SourceKind.Monitor !in filter.kinds) return emptyList()
        return displays.all().map(DisplayFacts::source)
    }

    override suspend fun sources(filter: SourceFilter): SourceList =
        SourceList(listSources(filter), SourceListing.Enumerable)

    override fun watch_sources(filter: SourceFilter): Flow<SourceChange> = callbackFlow {
        var current = listSources(filter)
        trySend(SourceChange.Listed(SourceList(current, SourceListing.Enumerable)))
        val listener = object : DisplayManager.DisplayListener {
            private fun relist() {
                val next = runCatching { listSources(filter) }.getOrElse { return }
                val before = current.associateBy { it.id }
                val after = next.associateBy { it.id }
                for (id in before.keys) if (id !in after) trySend(SourceChange.Removed(id))
                for ((id, source) in after) {
                    val old = before[id]
                    if (old == null) trySend(SourceChange.Added(source)) else if (old != source) trySend(SourceChange.Changed(source))
                }
                current = next
            }

            override fun onDisplayAdded(displayId: Int) = relist()

            override fun onDisplayRemoved(displayId: Int) = relist()

            override fun onDisplayChanged(displayId: Int) = relist()
        }
        displays.manager.registerDisplayListener(listener, handler)
        awaitClose { displays.manager.unregisterDisplayListener(listener) }
    }

    // ---- Access -------------------------------------------------------

    private fun granted(permission: String): Boolean =
        appContext.checkSelfPermission(permission) == PackageManager.PERMISSION_GRANTED

    /**
     * Runtime permission state with the camera plugin's heuristic: never
     * requested (or the system still offers a rationale) is
     * `NotDetermined`, requested and refused for good is `Denied`.
     */
    private fun permissionStatus(permission: String): AccessStatus {
        if (granted(permission)) return AccessStatus.Granted
        if (!permissionPrefs.getBoolean(permission, false)) return AccessStatus.NotDetermined
        val activity = activityRef.get() ?: return AccessStatus.Denied
        return if (activity.shouldShowRequestPermissionRationale(permission)) AccessStatus.NotDetermined else AccessStatus.Denied
    }

    private fun notificationStatus(): AccessStatus {
        if (Build.VERSION.SDK_INT >= Api.TIRAMISU) return permissionStatus(Manifest.permission.POST_NOTIFICATIONS)
        val manager = appContext.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        return if (manager.areNotificationsEnabled()) AccessStatus.Granted else AccessStatus.Denied
    }

    /** What the person sees while a projection runs. */
    private fun indicator(): IndicatorState =
        if (Build.VERSION.SDK_INT >= Api.TIRAMISU && !granted(Manifest.permission.POST_NOTIFICATIONS)) {
            // The status-bar chip (15 QPR1+) still shows.
            IndicatorState.Hidden("notification permission denied")
        } else {
            IndicatorState.Shown
        }

    override suspend fun access(scope: AccessScope): AccessState = when (scope) {
        AccessScope.ScreenRecording -> AccessState(
            scope, AccessStatus.PerSession, ConsentModel.PerSessionDialog, Persistence.Unsupported,
            revocableMidSession = true, indicator = indicator(),
        )
        AccessScope.Notifications -> AccessState(
            scope, notificationStatus(), ConsentModel.SystemSetting, Persistence.SystemSetting,
            revocableMidSession = true, indicator = IndicatorState.NotApplicable,
        )
        AccessScope.Microphone -> AccessState(
            scope, permissionStatus(Manifest.permission.RECORD_AUDIO), ConsentModel.SystemSetting,
            Persistence.SystemSetting, revocableMidSession = true, indicator = IndicatorState.NotApplicable,
        )
        AccessScope.BorderlessCapture -> AccessState(
            scope, AccessStatus.NotApplicable, ConsentModel.NoneNeeded, Persistence.Unsupported,
            revocableMidSession = false, indicator = IndicatorState.NotApplicable,
        )
    }

    override suspend fun request_access(request: AccessRequest): AccessState {
        val permission = when (request.scope) {
            AccessScope.Notifications ->
                if (Build.VERSION.SDK_INT >= Api.TIRAMISU) Manifest.permission.POST_NOTIFICATIONS else null
            AccessScope.Microphone -> Manifest.permission.RECORD_AUDIO
            AccessScope.ScreenRecording, AccessScope.BorderlessCapture -> null
        }
        if (permission != null && !granted(permission)) requestPermission(permission)
        return access(request.scope)
    }

    private suspend fun requestPermission(permission: String): Unit = withContext(Dispatchers.Main) {
        val activity = activityRef.get()?.takeUnless { it.isFinishing || it.isDestroyed }
            ?: fail(CaptureError.Protocol(BackendDetail(PROJECTION, "request_access", "no host activity")))
        suspendCancellableCoroutine { cont ->
            // Registered outside a lifecycle: unregistered by hand.
            lateinit var launcher: ActivityResultLauncher<String>
            launcher = activity.activityResultRegistry.register(
                "istmo.screen_capture.permission.${System.nanoTime()}",
                ActivityResultContracts.RequestPermission(),
            ) {
                launcher.unregister()
                permissionPrefs.edit().putBoolean(permission, true).apply()
                if (cont.isActive) cont.resume(Unit)
            }
            cont.invokeOnCancellation { launcher.unregister() }
            launcher.launch(permission)
        }
    }

    override suspend fun request_projection(request: ProjectionRequest): ProjectionGrant {
        if (request.mode == ProjectionMode.SingleApp && Build.VERSION.SDK_INT < Api.U) {
            fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Kind(SourceKind.Application)))))
        }
        return grants.put(consent.request(request.mode))
    }

    // ---- Planning -----------------------------------------------------

    /** The settled request, or the error naming what cannot be met. Shows no UI. */
    private fun plan(config: SessionConfig, frames: Boolean, audio: AudioConfig?): SessionPlan {
        val selection = Selection(config.backend)
        val display = displays.primary()
        val unmet = ArrayList<Requirement>()
        val target = target(config.source, display, config.consent, streaming = true, unmet)
        val video = config.video
        val format = if (frames) negotiate(video.formats, cpuFirst = false) else null
        if (frames && format == null) unmet += Requirement.Formats
        if (video.hdr == HdrPolicy.RequireHdr) unmet += Requirement.Hdr
        if (video.cursor.strength == Strength.Require && video.cursor.mode != CursorMode.Hidden) unmet += Requirement.Cursor
        if (video.damage == Strength.Require) unmet += Requirement.Damage
        if (frames && video.exhaustion == Exhaustion.CopyToCpu && format?.offer == Offer.Gpu) {
            unmet += Requirement.ExhaustionReadback
        }
        if (video.gpu is GpuPreference.Same) unmet += Requirement.Gpu
        if (config.windows.borderless == Borderless.Require) unmet += Requirement.Borderless
        if (unmet.isNotEmpty()) selection.unsatisfiable(unmet)
        val audioPlan = audio?.let { AudioPlan.of(appContext, it, selection) }
        consentGiven(config.consent)
        return SessionPlan(
            display = display,
            mode = target.mode,
            size = video.size,
            region = target.region,
            format = format,
            fps = video.fps,
            poolRequest = video.poolDepth,
            exhaustion = video.exhaustion,
            audio = audioPlan,
            indicator = indicator(),
        )
    }

    private class Target(val mode: ProjectionMode, val region: Rect?)

    private fun target(
        source: SourceSelector,
        display: DisplayFacts,
        consent: ConsentConfig,
        streaming: Boolean,
        unmet: MutableList<Requirement>,
    ): Target {
        val multi = if (streaming) Requirement.CompositeStreaming else Requirement.MultiMonitor
        return when (source) {
            SourceSelector.PrimaryMonitor -> Target(ProjectionMode.EntireScreen, null)
            is SourceSelector.Id -> {
                if (source.value != display.sourceId) {
                    if (displays.all().any { it.sourceId == source.value }) {
                        // Another display: MediaProjection mirrors the default one only.
                        unmet += Requirement.SourceSelector
                    } else {
                        fail(CaptureError.SourceNotFound(source.value))
                    }
                }
                Target(ProjectionMode.EntireScreen, null)
            }
            is SourceSelector.Region -> {
                val rect = regionPixels(display, source.value)
                if (rect == null) unmet += Requirement.Region
                Target(ProjectionMode.EntireScreen, rect)
            }
            is SourceSelector.Picker -> {
                val kinds = source.value.kinds
                if (!consent.allowPicker && consent.grant == null) unmet += Requirement.Picker
                if (SourceKind.Window in kinds) unmet += Requirement.Kind(SourceKind.Window)
                if (source.value.multiple) unmet += multi
                if (SourceKind.Application in kinds && Build.VERSION.SDK_INT < Api.U) {
                    unmet += Requirement.Kind(SourceKind.Application)
                }
                val apps = kinds.isEmpty() || SourceKind.Application in kinds
                Target(if (apps && Build.VERSION.SDK_INT >= Api.U) ProjectionMode.UserChoice else ProjectionMode.EntireScreen, null)
            }
            SourceSelector.AllMonitors -> {
                unmet += multi
                Target(ProjectionMode.EntireScreen, null)
            }
            is SourceSelector.Monitors -> {
                if (source.value != listOf(display.sourceId)) unmet += multi
                Target(ProjectionMode.EntireScreen, null)
            }
            SourceSelector.Focused, SourceSelector.UnderCursor -> {
                unmet += Requirement.SourceSelector
                Target(ProjectionMode.EntireScreen, null)
            }
        }
    }

    /** A grant, or leave to show the dialog; neither is `ConsentRequired`. */
    private fun consentGiven(consent: ConsentConfig) {
        if (consent.grant == null && !consent.allowPicker) fail(CaptureError.ConsentRequired)
    }

    /** The grant's projection, or a new one from the dialog. */
    private suspend fun projection(consent: ConsentConfig, mode: ProjectionMode): Projection =
        consent.grant?.let(grants::take) ?: this.consent.request(mode)

    // ---- Sessions -----------------------------------------------------

    /** Close the sinks with the failure as their end reason when [block] throws, then rethrow. */
    private suspend fun <T> closingOnFailure(vararg sinks: IstmoFrameSender<*>, block: suspend () -> T): T = try {
        block()
    } catch (e: Throwable) {
        val reason = (e as? BackendException)?.error?.toString() ?: e.toString()
        for (sink in sinks) sink.closeWithError(reason)
        throw e
    }

    private suspend fun openSession(
        config: SessionConfig,
        video: IstmoFrameSender<FrameMeta>?,
        preview: PreviewTarget?,
        audio: IstmoFrameSender<AudioMeta>?,
    ): CaptureSession {
        val surface = preview?.let(::surfaceOf)
        val audioConfig = if (audio != null) config.audio ?: DEFAULT_AUDIO else null
        val plan = plan(config, frames = video != null, audio = audioConfig)
        val projection = projection(config.consent, plan.mode)
        val id = IstmoRuntime.allocHandleId(ScreenCaptureDispatcher.PLUGIN_ID)
        val session = ProjectionSession(
            id = id,
            handler = handler,
            displays = displays,
            projection = projection,
            plan = plan,
            video = video,
            preview = surface,
            fit = preview?.fit ?: PreviewFit.Fit,
            audioSender = audio,
            playback = { audioPlan, sender, onEnd -> PlaybackAudio(appContext, audioPlan, projection.projection, sender, onEnd) },
            onEnded = { ended ->
                sessions.remove(ended.id)
                IstmoRuntime.forgetHandle(ended.id)
            },
        )
        sessions[id] = session
        val negotiated = try {
            session.start()
        } catch (e: Throwable) {
            sessions.remove(id)
            IstmoRuntime.forgetHandle(id)
            throw e
        }
        return CaptureSession(id, negotiated)
    }

    private fun surfaceOf(target: PreviewTarget): Surface {
        if (target.kind != PreviewSurfaceKind.AndroidSurface) {
            fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Operation))))
        }
        val surface = IstmoRuntime.objectAny(target.surface) as? Surface ?: fail(CaptureError.SinkUnreachable)
        if (!surface.isValid) fail(CaptureError.SinkUnreachable)
        return surface
    }

    override suspend fun open(config: SessionConfig, video: IstmoFrameSender<FrameMeta>): CaptureSession =
        closingOnFailure(video) { openSession(config, video, null, null) }

    override suspend fun open_with_audio(
        config: SessionConfig,
        video: IstmoFrameSender<FrameMeta>,
        audio: IstmoFrameSender<AudioMeta>,
    ): CaptureSession = closingOnFailure(video, audio) { openSession(config, video, null, audio) }

    override suspend fun open_preview(config: SessionConfig, target: PreviewTarget): CaptureSession =
        openSession(config, null, target, null)

    override suspend fun open_preview_with_frames(
        config: SessionConfig,
        target: PreviewTarget,
        video: IstmoFrameSender<FrameMeta>,
    ): CaptureSession = closingOnFailure(video) { openSession(config, video, target, null) }

    override suspend fun update(session: NativeHandleId, update: SessionUpdate): NegotiatedStream {
        val live = sessions[session] ?: fail(CaptureError.SessionNotFound)
        return live.update(update)
    }

    override suspend fun close(session: NativeHandleId) {
        val live = sessions[session] ?: fail(CaptureError.SessionNotFound)
        live.close()
    }

    override fun events(session: NativeHandleId): Flow<CaptureEvent> = flow {
        val live = sessions[session] ?: fail(CaptureError.SessionNotFound)
        emitAll(live.hub.flow())
    }

    override fun releaseNativeHandle(handleId: Long) {
        // Every release reaches every releaser; ids this backend never
        // handed out are in neither table.
        sessions[handleId]?.let { scope.launch { it.close() } }
        audioSessions[handleId]?.let { scope.launch { it.close() } }
    }

    /** The notification's Stop action: every projection ends as revoked. */
    private fun stopFromNotification() {
        handler.post {
            for (session in sessions.values) session.end(StopReason.ConsentRevoked, STOPPED_BY_USER)
            for (session in audioSessions.values) session.end(StopReason.ConsentRevoked, STOPPED_BY_USER)
            grants.stopAll()
        }
    }

    // ---- Screenshots --------------------------------------------------

    private fun planShot(request: ScreenshotRequest, cpu: Boolean): SessionPlan {
        val selection = Selection(request.backend)
        val display = displays.primary()
        val unmet = ArrayList<Requirement>()
        val target = target(request.source, display, request.consent, streaming = false, unmet)
        val format = if (cpu) Negotiated(Offer.Cpu, 0) else negotiate(request.formats, cpuFirst = true)
        if (format == null) unmet += Requirement.Formats
        if (request.hdr == HdrPolicy.RequireHdr) unmet += Requirement.Hdr
        if (request.cursor.strength == Strength.Require && request.cursor.mode != CursorMode.Hidden) {
            unmet += Requirement.Cursor
        }
        if (unmet.isNotEmpty()) selection.unsatisfiable(unmet)
        consentGiven(request.consent)
        return SessionPlan(
            display = display,
            mode = target.mode,
            size = OutputSize.Native,
            region = target.region,
            format = format,
            fps = FrameRate.Native,
            poolRequest = 1u,
            exhaustion = Exhaustion.Stall,
            audio = null,
            indicator = indicator(),
        )
    }

    override suspend fun screenshot(request: ScreenshotRequest, sink: IstmoFrameSender<FrameMeta>): ScreenshotInfo =
        closingOnFailure(sink) {
            val plan = planShot(request, cpu = false)
            val projection = projection(request.consent, plan.mode)
            val delivered = CompletableDeferred<Unit>()
            val session = ProjectionSession(
                id = 0L,
                handler = handler,
                displays = displays,
                projection = projection,
                plan = plan,
                video = sink,
                preview = null,
                fit = PreviewFit.Fit,
                audioSender = null,
                playback = { _, _, _ -> fail(CaptureError.Unsatisfiable(Unmet(PROJECTION, listOf(Requirement.Audio)))) },
                onEnded = {},
            )
            session.onFirstFrame = { delivered.complete(Unit) }
            val negotiated = session.start()
            try {
                withTimeout(SHOT_TIMEOUT_MS) { delivered.await() }
            } catch (e: TimeoutCancellationException) {
                val error = CaptureError.Protocol(BackendDetail(PROJECTION, "screenshot", "no frame within 5 s"))
                handler.call { session.end(StopReason.Failed, error.toString()) }
                fail(error)
            }
            // Ends the stream after the queued frame; a held
            // `HardwareBuffer` keeps its reader until Rust drops it.
            session.close()
            ScreenshotInfo(1u, PROJECTION, negotiated)
        }

    override suspend fun screenshot_pixels(request: ScreenshotRequest, layout: PixelLayout): RawImage {
        if (layout.format == CpuPixelFormat.Rgba16f) Selection(request.backend).unsatisfiable(Requirement.Formats)
        val plan = planShot(request, cpu = true)
        val projection = projection(request.consent, plan.mode)
        if (!projection.used.compareAndSet(false, true)) fail(CaptureError.GrantExpired)
        return try {
            grabPixels(projection, plan, layout)
        } finally {
            projection.stop()
        }
    }

    /** One CPU image straight from a reader, cropped and in the asked byte order. */
    private suspend fun grabPixels(projection: Projection, plan: SessionPlan, layout: PixelLayout): RawImage {
        val display = plan.display
        val result = CompletableDeferred<RawImage>()
        val reader = ImageReader.newInstance(display.width, display.height, android.graphics.PixelFormat.RGBA_8888, 2)
        reader.setOnImageAvailableListener({ source ->
            val image = runCatching { source.acquireLatestImage() }.getOrNull() ?: return@setOnImageAvailableListener
            try {
                if (!result.isCompleted) result.complete(pixelsOf(image, plan, layout))
            } catch (e: RuntimeException) {
                result.completeExceptionally(e)
            } finally {
                image.close()
            }
        }, handler)
        val virtualDisplay = handler.call {
            projection.projection.createVirtualDisplay(
                ProjectionSession.VIRTUAL_DISPLAY_NAME, display.width, display.height, display.densityDpi,
                DisplayManager.VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR, reader.surface, null, handler,
            )
        }
        return try {
            withTimeout(SHOT_TIMEOUT_MS) { result.await() }
        } catch (e: TimeoutCancellationException) {
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "screenshot_pixels", "no frame within 5 s")))
        } finally {
            handler.post {
                virtualDisplay?.release()
                reader.close()
            }
        }
    }

    private fun pixelsOf(image: android.media.Image, plan: SessionPlan, layout: PixelLayout): RawImage {
        val plane = image.planes[0]
        val crop = plan.region ?: Rect(0, 0, image.width.toUInt(), image.height.toUInt())
        val width = crop.width.toInt()
        val height = crop.height.toInt()
        val rowBytes = width * 4
        val stride = if (layout.tight) rowBytes else plane.rowStride
        val out = ByteArray(stride * height)
        val src = plane.buffer.duplicate()
        for (row in 0 until height) {
            src.position((crop.y + row) * plane.rowStride + crop.x * plane.pixelStride)
            src.get(out, row * stride, rowBytes)
        }
        if (layout.format == CpuPixelFormat.Bgra8) {
            for (row in 0 until height) {
                var i = row * stride
                val end = i + rowBytes
                while (i < end) {
                    val red = out[i]
                    out[i] = out[i + 2]
                    out[i + 2] = red
                    i += 4
                }
            }
        }
        return RawImage(width.toUInt(), height.toUInt(), stride.toUInt(), layout.format, SRGB, plan.display.sourceId, out)
    }

    // ---- Audio-only sessions --------------------------------------------

    private inner class AudioSession(
        val id: Long,
        private val projection: Projection,
        private val plan: AudioPlan,
        private val sender: IstmoFrameSender<AudioMeta>,
    ) : ProjectionListener {
        val hub = EventHub<AudioEvent>(AudioEvent.Started(plan.negotiated))
        private val capture = PlaybackAudio(appContext, plan, projection.projection, sender) { error ->
            handler.post { end(if (error == null) StopReason.Closed else StopReason.Failed, error?.toString(), error) }
        }
        private var ended = false
        private val done = CompletableDeferred<Unit>()

        fun start() {
            projection.listener = this
            if (projection.isStopped) fail(CaptureError.ConsentRevoked)
            capture.start()
        }

        fun update(update: AudioUpdate): AudioNegotiated {
            when (update) {
                AudioUpdate.Pause -> {
                    capture.paused = true
                    hub.publish(AudioEvent.Paused(PauseReason.Requested))
                }
                AudioUpdate.Resume -> {
                    capture.paused = false
                    hub.publish(AudioEvent.Resumed)
                }
                AudioUpdate.Mute -> {
                    capture.muted = true
                    hub.publish(AudioEvent.Muted)
                }
                AudioUpdate.Unmute -> {
                    capture.muted = false
                    hub.publish(AudioEvent.Unmuted)
                }
            }
            return plan.negotiated
        }

        override fun onProjectionStop() {
            handler.post { end(StopReason.ConsentRevoked, STOPPED_BY_USER) }
        }

        override fun onContentResize(width: Int, height: Int) {}

        override fun onContentVisible(visible: Boolean) {}

        suspend fun close() {
            handler.post { end(StopReason.Closed, null) }
            done.await()
        }

        fun end(reason: StopReason, error: String?, failure: CaptureError? = null) {
            if (ended) return
            ended = true
            projection.listener = null
            capture.stop()
            projection.stop()
            if (error == null) sender.close() else sender.closeWithError(error)
            failure?.let { hub.publish(AudioEvent.Failed(it)) }
            hub.publish(AudioEvent.Stopped(reason))
            hub.close()
            audioSessions.remove(id)
            IstmoRuntime.forgetHandle(id)
            done.complete(Unit)
        }
    }

    override suspend fun open_audio(config: AudioConfig, audio: IstmoFrameSender<AudioMeta>): AudioCaptureSession =
        closingOnFailure(audio) {
            val selection = Selection(BackendSelection.Auto(AutoSelect(AutoPreference.ZeroCopy, emptyList())))
            val plan = AudioPlan.of(appContext, config, selection)
            // `AudioConfig` carries no consent: the projection comes from
            // an earlier `request_projection`.
            val projection = grants.takeAny() ?: fail(CaptureError.ConsentRequired)
            if (!projection.used.compareAndSet(false, true)) fail(CaptureError.GrantExpired)
            val id = IstmoRuntime.allocHandleId(ScreenCaptureDispatcher.PLUGIN_ID)
            val session = AudioSession(id, projection, plan, audio)
            audioSessions[id] = session
            try {
                handler.call { session.start() }
            } catch (e: Throwable) {
                audioSessions.remove(id)
                IstmoRuntime.forgetHandle(id)
                projection.stop()
                throw e
            }
            AudioCaptureSession(id, plan.negotiated)
        }

    override suspend fun update_audio(session: NativeHandleId, update: AudioUpdate): AudioNegotiated {
        val live = audioSessions[session] ?: fail(CaptureError.SessionNotFound)
        return handler.call { live.update(update) }
    }

    override suspend fun close_audio(session: NativeHandleId) {
        val live = audioSessions[session] ?: fail(CaptureError.SessionNotFound)
        live.close()
    }

    override fun audio_events(session: NativeHandleId): Flow<AudioEvent> = flow {
        val live = audioSessions[session] ?: fail(CaptureError.SessionNotFound)
        emitAll(live.hub.flow())
    }

    private companion object {
        const val SHOT_TIMEOUT_MS = 5_000L
        const val STOPPED_BY_USER = "screen capture consent revoked"
        val DEFAULT_AUDIO = AudioConfig(AudioSource.System, null, null, SampleFormat.S16)
    }
}
