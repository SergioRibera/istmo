package dev.istmo.plugins.screencapture

import android.app.Activity
import android.content.Context
import android.content.Intent
import android.media.projection.MediaProjection
import android.media.projection.MediaProjectionConfig
import android.media.projection.MediaProjectionManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResult
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import java.lang.ref.WeakReference
import java.security.SecureRandom
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicBoolean
import kotlin.coroutines.resume
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.isActive
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.withContext

/** What a running projection tells its user. */
internal interface ProjectionListener {
    /** The projection ended: the person, the system (lock, another projection) or [Projection.stop]. */
    fun onProjectionStop()

    /** API 34+: the captured region (an app window, or the display) changed size. */
    fun onContentResize(width: Int, height: Int)

    /** API 34+: the captured app became hidden or visible. */
    fun onContentVisible(visible: Boolean)
}

/**
 * A `MediaProjection` the person consented to, running under the
 * foreground service. Its callback is registered before any virtual
 * display exists (required on API 34+); the session using it installs
 * itself as [listener].
 */
internal class Projection(
    private val context: Context,
    val projection: MediaProjection,
    val mode: ProjectionMode,
    handler: Handler,
) {
    private val ended = AtomicBoolean(false)

    @Volatile
    var listener: ProjectionListener? = null

    /** Set once a virtual display or an audio capture used the projection (single use). */
    val used = AtomicBoolean(false)

    val isStopped: Boolean get() = ended.get()

    private val callback = object : MediaProjection.Callback() {
        override fun onStop() {
            if (ended.compareAndSet(false, true)) ProjectionForeground.release(context)
            listener?.onProjectionStop()
        }

        override fun onCapturedContentResize(width: Int, height: Int) {
            listener?.onContentResize(width, height)
        }

        override fun onCapturedContentVisibilityChanged(isVisible: Boolean) {
            listener?.onContentVisible(isVisible)
        }
    }

    init {
        projection.registerCallback(callback, handler)
    }

    /** End the projection; `onStop` follows on the projection's handler. */
    fun stop() {
        if (ended.get()) return
        runCatching { projection.stop() }
        // `stop` normally calls back `onStop`; release the service lease
        // here too in case the projection was already dead.
        if (ended.compareAndSet(false, true)) ProjectionForeground.release(context)
    }
}

/**
 * The consent dialog (`createScreenCaptureIntent`) through the host
 * activity's `ActivityResultRegistry`, and the foreground service a
 * granted projection needs.
 *
 * A consent still showing when the host activity is recreated (a
 * rotation) is registered again on the new activity under the same key,
 * so the registry hands the result to it instead of dropping it.
 */
internal class ProjectionConsent(
    private val context: Context,
    private val handler: Handler,
) {
    private val manager: MediaProjectionManager? =
        context.getSystemService(Context.MEDIA_PROJECTION_SERVICE) as? MediaProjectionManager

    @Volatile
    private var activityRef: WeakReference<ComponentActivity>? = null
    private val pending = ConcurrentHashMap<String, Pending>()
    private val main = Handler(Looper.getMainLooper())

    val available: Boolean get() = manager != null

    private inner class Pending(val key: String, val onResult: (ActivityResult) -> Unit) {
        @Volatile
        var launcher: ActivityResultLauncher<Intent>? = null

        fun register(activity: ComponentActivity): ActivityResultLauncher<Intent> {
            val registered = activity.activityResultRegistry.register(
                key,
                ActivityResultContracts.StartActivityForResult(),
            ) { result ->
                pending.remove(key)
                launcher?.unregister()
                onResult(result)
            }
            launcher = registered
            return registered
        }
    }

    fun attach(activity: ComponentActivity) {
        activityRef = WeakReference(activity)
        if (pending.isEmpty()) return
        // After `onCreate` finished, so the registry restored the keys it
        // saved and delivers a result that arrived for the old activity.
        main.post {
            for (consent in pending.values) consent.register(activity)
        }
    }

    private fun activity(): ComponentActivity =
        activityRef?.get()?.takeUnless { it.isFinishing || it.isDestroyed }
            ?: fail(CaptureError.Protocol(BackendDetail(PROJECTION, "consent", "no host activity to show the consent dialog")))

    private fun intent(mode: ProjectionMode): Intent {
        val manager = manager ?: fail(CaptureError.BackendUnavailable(BackendReason(PROJECTION, unavailable())))
        if (Build.VERSION.SDK_INT >= Api.U) {
            val config = when (mode) {
                ProjectionMode.EntireScreen -> MediaProjectionConfig.createConfigForDefaultDisplay()
                // The system dialog offers both; Android has no config
                // restricted to apps.
                ProjectionMode.UserChoice, ProjectionMode.SingleApp -> MediaProjectionConfig.createConfigForUserChoice()
            }
            return manager.createScreenCaptureIntent(config)
        }
        return manager.createScreenCaptureIntent()
    }

    /** Availability of the projection service itself. */
    fun unavailable(): Availability = Availability.NotOnThisSession("no MediaProjectionManager on this device")

    /**
     * Show the consent dialog, then start the foreground service and
     * the projection. `ConsentDenied` when the person declines.
     */
    suspend fun request(mode: ProjectionMode): Projection {
        val intent = intent(mode)
        val result = withContext(Dispatchers.Main) {
            val activity = activity()
            suspendCancellableCoroutine { cont ->
                val key = "istmo.screen_capture.consent.${System.nanoTime()}"
                val consent = Pending(key) { result -> if (cont.isActive) cont.resume(result) }
                pending[key] = consent
                val launcher = consent.register(activity)
                cont.invokeOnCancellation {
                    pending.remove(key)
                    main.post { consent.launcher?.unregister() }
                }
                try {
                    launcher.launch(intent)
                } catch (e: RuntimeException) {
                    pending.remove(key)
                    launcher.unregister()
                    throw e
                }
            }
        }
        val data = result.data
        if (result.resultCode != Activity.RESULT_OK || data == null) fail(CaptureError.ConsentDenied)
        return start(result.resultCode, data, mode)
    }

    private suspend fun start(resultCode: Int, data: Intent, mode: ProjectionMode): Projection {
        val manager = manager ?: fail(CaptureError.BackendUnavailable(BackendReason(PROJECTION, unavailable())))
        try {
            ProjectionForeground.acquire(context)
        } catch (e: kotlinx.coroutines.TimeoutCancellationException) {
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "foreground service", "did not start within 5 s")))
        } catch (e: CancellationException) {
            // A `java.lang.IllegalStateException` too: the caller's
            // cancellation, not a service failure (`acquire` gave its lease
            // back).
            throw e
        } catch (e: RuntimeException) {
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "foreground service", e.toString())))
        }
        // Not cancellable: a projection the system already handed out must
        // reach an owner that stops it.
        val projection = try {
            withContext(NonCancellable + Dispatchers.Main) {
                manager.getMediaProjection(resultCode, data)?.let { Projection(context, it, mode, handler) }
            }
        } catch (e: RuntimeException) {
            ProjectionForeground.release(context)
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "getMediaProjection", e.toString())))
        }
        if (projection == null) {
            ProjectionForeground.release(context)
            fail(CaptureError.ConsentDenied)
        }
        if (!currentCoroutineContext().isActive) {
            // Cancelled meanwhile: nobody will use it.
            projection.stop()
            throw CancellationException("cancelled after the consent")
        }
        return projection
    }
}

/**
 * Projections granted by `request_projection`, waiting for the `open` /
 * `screenshot` that names them. Single use, gone on the projection's
 * `onStop` or after [TTL_MS]. Ids are random so another process cannot
 * guess one; only the id ever leaves the process.
 */
internal class ProjectionGrants(private val handler: Handler) {
    private val grants = ConcurrentHashMap<Long, Projection>()
    private val random = SecureRandom()

    fun put(projection: Projection): ProjectionGrant {
        val id = (random.nextLong() and Long.MAX_VALUE) or GRANT_ID_FLOOR
        grants[id] = projection
        projection.listener = object : ProjectionListener {
            override fun onProjectionStop() {
                grants.remove(id)
            }

            override fun onContentResize(width: Int, height: Int) {}

            override fun onContentVisible(visible: Boolean) {}
        }
        handler.postDelayed({ grants.remove(id)?.stop() }, TTL_MS)
        return ProjectionGrant(id)
    }

    /** The grant's projection, removed; `GrantExpired` when it is gone or was used. */
    fun take(grant: ProjectionGrant): Projection {
        val projection = grants.remove(grant.handle) ?: fail(CaptureError.GrantExpired)
        if (projection.isStopped) fail(CaptureError.GrantExpired)
        projection.listener = null
        return projection
    }

    /**
     * The one live grant, removed (audio-only sessions carry no grant
     * field); `null` without one. With several waiting, which one the
     * caller meant is unknown (another may be for a video `open`): none
     * is taken.
     */
    fun takeOnly(): Projection? {
        synchronized(this) {
            val live = grants.entries.filter { !it.value.isStopped }
            if (live.size > 1) {
                fail(
                    CaptureError.Protocol(
                        BackendDetail(PROJECTION, "open_audio", "${live.size} grants wait: use each before asking for another"),
                    ),
                )
            }
            val (id, projection) = live.firstOrNull() ?: return null
            if (grants.remove(id) == null) return null
            projection.listener = null
            return projection
        }
    }

    /** Whether a grant still holds a running projection. */
    fun anyLive(): Boolean = grants.values.any { !it.isStopped }

    fun stopAll() {
        val held = grants.values.toList()
        grants.clear()
        held.forEach(Projection::stop)
    }

    private companion object {
        const val TTL_MS = 60_000L

        /** Above the runtime's sequential handle counter. */
        const val GRANT_ID_FLOOR = 0x4000_0000_0000_0000L
    }
}
