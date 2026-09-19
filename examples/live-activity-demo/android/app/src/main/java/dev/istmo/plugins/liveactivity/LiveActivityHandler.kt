package dev.istmo.plugins.liveactivity

import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import dev.istmo.runtime.ActivityError
import dev.istmo.runtime.NativeHandleId

interface LiveActivityHandler {

    val activityType: String

    val channel: ChannelConfig

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

    suspend fun restoreActive(): List<dev.istmo.runtime.RestoredActivity> = emptyList()

    fun releaseHandle(handle: NativeHandleId)
}

data class ChannelConfig(
    val id: String,
    val name: String,
    val importance: Int = NotificationManager.IMPORTANCE_LOW,
    val description: String? = null,
) {

    fun toChannel(): NotificationChannel {
        val channel = NotificationChannel(id, name, importance)
        description?.let { channel.description = it }
        return channel
    }

    companion object {

        fun default(): ChannelConfig = ChannelConfig(
            id = "istmo.live_activity",
            name = "Live activities",
            importance = NotificationManager.IMPORTANCE_LOW,
            description = "Ongoing status updates.",
        )
    }
}

