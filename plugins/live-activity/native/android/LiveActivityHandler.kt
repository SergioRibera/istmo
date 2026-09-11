package dev.istmo.plugins.liveactivity

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import dev.istmo.runtime.ActivityError
import dev.istmo.runtime.NativeHandleId

/**
 * Type-erased handler surface consumed by [LiveActivityBackendImpl].
 *
 * One implementation lives on the consumer app side per activity type
 * (`"timer"`, `"delivery"`, `"workout"`). The base backend routes every
 * wire call to the handler registered for its `activity_type` string and
 * tracks the [NativeHandleId] ↔ activity-type mapping.
 *
 * Handlers most commonly extend [NotificationLiveActivityHandler] which
 * supplies the notification / channel plumbing on top of a
 * [RenderStrategy]. Custom handlers (e.g. a foreground-service-backed
 * activity) can implement this interface directly.
 */
interface LiveActivityHandler {

    /** Wire identifier this handler services. */
    val activityType: String

    /** Notification channel configuration; created lazily by the backend. */
    val channel: ChannelConfig

    /**
     * Start a new activity, returning a freshly issued [NativeHandleId].
     * Backend calls [dev.istmo.runtime.IstmoRuntime.allocHandleId] to
     * mint the id, then hands it here so the handler can associate any
     * platform-side state.
     *
     * Throw [dev.istmo.runtime.BackendException] wrapping an
     * [ActivityError] for typed failures.
     */
    suspend fun start(
        handle: NativeHandleId,
        attributes: ByteArray,
        initialState: ByteArray,
        style: dev.istmo.runtime.ActivityStyle,
        staleAfterSeconds: UInt?,
        androidTierHint: dev.istmo.runtime.AndroidTierHint?,
    )

    suspend fun update(
        handle: NativeHandleId,
        state: ByteArray,
        alert: dev.istmo.runtime.AlertConfig?,
    )

    suspend fun end(
        handle: NativeHandleId,
        finalState: ByteArray?,
        dismissal: dev.istmo.runtime.DismissalPolicy,
    )

    /**
     * Reattach to activities of this type that survived a process
     * restart. Return an empty list when nothing was persisted.
     *
     * Android does not persist ongoing notifications across process
     * death by default; this hook is here so authors who tie their
     * activities to a foreground service (or a `WorkManager`
     * `CoroutineWorker`) can rebuild the handle registry after resume.
     */
    suspend fun restoreActive(): List<dev.istmo.runtime.RestoredActivity> = emptyList()

    /**
     * Invoked by the backend when the Rust side drops its
     * `NativeHandle<LiveActivityToken>`. Treat as an immediate end.
     */
    fun releaseHandle(handle: NativeHandleId)
}

/**
 * Notification-channel configuration produced by a
 * [LiveActivityHandler]. The backend creates the channel lazily the
 * first time an activity of this handler's type starts.
 */
data class ChannelConfig(
    val id: String,
    val name: String,
    val importance: Int = NotificationManager.IMPORTANCE_LOW,
    val description: String? = null,
) {

    /** Materialise a [NotificationChannel] to hand to the platform. */
    fun toChannel(): NotificationChannel {
        val channel = NotificationChannel(id, name, importance)
        description?.let { channel.description = it }
        return channel
    }

    companion object {
        /** Reasonable default channel for one-off consumer apps. */
        fun default(): ChannelConfig = ChannelConfig(
            id = "istmo.live_activity",
            name = "Live activities",
            importance = NotificationManager.IMPORTANCE_LOW,
            description = "Ongoing status updates.",
        )
    }
}
