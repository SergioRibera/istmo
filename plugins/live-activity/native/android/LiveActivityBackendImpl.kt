package dev.istmo.plugins.liveactivity

import android.content.Context
import android.os.Build
import androidx.core.app.NotificationManagerCompat
import dev.istmo.runtime.ActivityError
import dev.istmo.runtime.ActivityStyle
import dev.istmo.runtime.AlertConfig
import dev.istmo.runtime.AndroidCapabilities
import dev.istmo.runtime.AndroidTierHint
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.DismissalPolicy
import dev.istmo.runtime.HandleReleaser
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.LiveActivityBackend
import dev.istmo.runtime.NativeHandleId
import dev.istmo.runtime.PlatformCapabilities
import dev.istmo.runtime.RestoredActivity
import java.util.concurrent.ConcurrentHashMap

/**
 * Reference [LiveActivityBackend] implementation.
 *
 * Routes every wire call to a per-activity-type [LiveActivityHandler]
 * registered by the consumer app. The plugin ships this class so a
 * typical app writes only its concrete handlers — routing, handle-id
 * allocation, capability probes and the runtime `HandleReleaser` hookup
 * come for free.
 *
 * Wiring in the app's `Application` / `Activity`:
 *
 * ```kotlin
 * val backend = LiveActivityBackendImpl(applicationContext).apply {
 *     register(TimerLiveActivityHandler(applicationContext))
 * }
 * IstmoRuntime.registerHandler(
 *     LiveActivityDispatcher.PLUGIN_ID,
 *     LiveActivityDispatcher(backend, LiveActivityCodecsImpl()),
 * )
 * ```
 */
class LiveActivityBackendImpl(
    private val appContext: Context,
) : LiveActivityBackend, HandleReleaser {

    private val handlersByType = ConcurrentHashMap<String, LiveActivityHandler>()
    private val typeByHandle = ConcurrentHashMap<Long, String>()

    /** Register a handler for its declared [LiveActivityHandler.activityType]. */
    fun register(handler: LiveActivityHandler) {
        handlersByType[handler.activityType] = handler
    }

    /** Remove a previously-registered handler. In-flight handles keep routing. */
    fun unregister(activityType: String) {
        handlersByType.remove(activityType)
    }

    // ---- LiveActivityBackend --------------------------------------------

    override suspend fun start(
        activity_type: String,
        attributes: ByteArray,
        initial_state: ByteArray,
        style: ActivityStyle,
        stale_after_seconds: UInt?,
        android_tier_hint: AndroidTierHint?,
    ): NativeHandleId {
        val handler = handlerFor(activity_type)
        val id: NativeHandleId = IstmoRuntime.allocHandleId(activity_type)
        try {
            handler.start(
                handle = id,
                attributes = attributes,
                initialState = initial_state,
                style = style,
                staleAfterSeconds = stale_after_seconds,
                androidTierHint = android_tier_hint,
            )
        } catch (e: Throwable) {
            IstmoRuntime.forgetHandle(id)
            throw e
        }
        typeByHandle[id] = activity_type
        return id
    }

    override suspend fun update(
        handle: NativeHandleId,
        state: ByteArray,
        alert: AlertConfig?,
    ) {
        val handler = handlerForHandle(handle)
        handler.update(handle, state, alert)
    }

    override suspend fun end(
        handle: NativeHandleId,
        final_state: ByteArray?,
        dismissal: DismissalPolicy,
    ) {
        val handler = handlerForHandle(handle)
        handler.end(handle, final_state, dismissal)
        typeByHandle.remove(handle)
        IstmoRuntime.forgetHandle(handle)
    }

    override suspend fun are_activities_enabled(): Boolean =
        NotificationManagerCompat.from(appContext).areNotificationsEnabled()

    override suspend fun capabilities(): PlatformCapabilities {
        val sdk = Build.VERSION.SDK_INT
        return PlatformCapabilities.Android(
            AndroidCapabilities(
                supports_custom = true,
                supports_progress_style = sdk >= 35,
                supports_live_update = sdk >= 36,
                notifications_enabled = NotificationManagerCompat
                    .from(appContext)
                    .areNotificationsEnabled(),
            ),
        )
    }

    override suspend fun restore_active(): List<RestoredActivity> {
        val out = mutableListOf<RestoredActivity>()
        for (handler in handlersByType.values) {
            val restored = handler.restoreActive()
            for (item in restored) {
                typeByHandle[item.handle] = handler.activityType
            }
            out.addAll(restored)
        }
        return out
    }

    // ---- HandleReleaser -------------------------------------------------

    override fun releaseNativeHandle(handleId: Long) {
        val ty = typeByHandle.remove(handleId) ?: return
        handlersByType[ty]?.releaseHandle(handleId)
    }

    // ---- Internal -------------------------------------------------------

    private fun handlerFor(activityType: String): LiveActivityHandler =
        handlersByType[activityType]
            ?: throw BackendException(ActivityError.UnknownActivityType(activityType))

    private fun handlerForHandle(handle: NativeHandleId): LiveActivityHandler {
        val ty = typeByHandle[handle]
            ?: throw BackendException(ActivityError.HandleNotFound)
        return handlerFor(ty)
    }
}
