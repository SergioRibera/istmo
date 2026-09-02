package dev.istmo.multi

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PluginException
import dev.istmo.runtime.PluginHandler
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * Kotlin handler for the `istmo.service_control` plugin. Every method
 * `ServiceContext` invokes on the Rust side round-trips to this class via
 * the frame protocol; here we translate to real Android APIs.
 */
class ServiceControlImpl(private val appContext: Context) : PluginHandler {

    private val wakelocks = ConcurrentHashMap<Long, PowerManager.WakeLock>()
    private val nextToken = AtomicLong(1L)

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "start_foreground" -> handleStartForeground(payload)
        "update_notification" -> handleUpdateNotification(payload)
        "stop_foreground" -> handleStopForeground(payload)
        "acquire_wakelock" -> handleAcquireWakelock(payload)
        "release_wakelock" -> handleReleaseWakelock(payload)
        "stop_self" -> handleStopSelf(payload)
        else -> throw PluginException(encodePlatformError("unknown service_control method: $method"))
    }

    // ---- Method handlers -----------------------------------------------

    private fun handleStartForeground(payload: ByteArray): ByteArray {
        val (serviceId, spec) = decodeServiceIdAndSpec(payload)
        val service = requireService(serviceId)
        ensureChannel(spec.channelId)
        val notification = buildNotification(spec)
        val type = spec.foregroundServiceType
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && type != null) {
            service.startForeground(spec.notificationId, notification, type)
        } else {
            @Suppress("DEPRECATION")
            service.startForeground(spec.notificationId, notification)
        }
        return EMPTY_OK
    }

    private fun handleUpdateNotification(payload: ByteArray): ByteArray {
        val (serviceId, spec) = decodeServiceIdAndSpec(payload)
        // Even during an update the service must exist; if it doesn't the
        // notification would be a leak.
        requireService(serviceId)
        ensureChannel(spec.channelId)
        val nm = appContext.getSystemService(NotificationManager::class.java)
        nm.notify(spec.notificationId, buildNotification(spec))
        return EMPTY_OK
    }

    private fun handleStopForeground(payload: ByteArray): ByteArray {
        val (serviceIdDecoded, cursorAfterId) = Bincode.readString(payload, 0)
        val (remove, _) = Bincode.readBool(payload, cursorAfterId)
        val service = requireService(serviceIdDecoded)
        val flags = if (remove) Service.STOP_FOREGROUND_REMOVE else Service.STOP_FOREGROUND_DETACH
        service.stopForeground(flags)
        return EMPTY_OK
    }

    private fun handleAcquireWakelock(payload: ByteArray): ByteArray {
        val (_, tag) = decodeServiceIdAndString(payload)
        val powerManager = appContext.getSystemService(PowerManager::class.java)
        val lock = powerManager.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "istmo:$tag")
        try {
            lock.acquire()
        } catch (t: SecurityException) {
            throw PluginException(encodeWakelockDenied(t.message ?: "SecurityException"))
        }
        val token = nextToken.getAndIncrement()
        wakelocks[token] = lock
        val out = ByteArrayOutputStream()
        Bincode.writeVarintU64(out, token)
        return out.toByteArray()
    }

    private fun handleReleaseWakelock(payload: ByteArray): ByteArray {
        val (_, cursorAfterId) = Bincode.readString(payload, 0)
        val (token, _) = Bincode.readVarintU64(payload, cursorAfterId)
        wakelocks.remove(token)?.let {
            if (it.isHeld) it.release()
        }
        return EMPTY_OK
    }

    private fun handleStopSelf(payload: ByteArray): ByteArray {
        val (serviceId, _) = Bincode.readString(payload, 0)
        requireService(serviceId).stopSelf()
        return EMPTY_OK
    }

    // ---- Helpers -------------------------------------------------------

    private fun requireService(serviceId: String): Service =
        IstmoRuntime.service(serviceId)
            ?: throw PluginException(encodeUnknownService(serviceId))

    private fun ensureChannel(channelId: String) {
        val nm = appContext.getSystemService(NotificationManager::class.java)
        if (nm.getNotificationChannel(channelId) != null) return
        val channel = NotificationChannel(
            channelId,
            channelId,
            NotificationManager.IMPORTANCE_LOW,
        )
        nm.createNotificationChannel(channel)
    }

    private fun buildNotification(spec: NotificationSpec): Notification {
        val icon = appContext.applicationInfo.icon
        return NotificationCompat.Builder(appContext, spec.channelId)
            .setContentTitle(spec.title)
            .setContentText(spec.body)
            .setSmallIcon(if (icon != 0) icon else android.R.drawable.stat_notify_sync)
            .setOngoing(spec.ongoing)
            .build()
    }

    private fun decodeServiceIdAndSpec(payload: ByteArray): Pair<String, NotificationSpec> {
        val (serviceId, next) = Bincode.readString(payload, 0)
        val spec = readNotificationSpec(payload, next)
        return serviceId to spec
    }

    private fun decodeServiceIdAndString(payload: ByteArray): Pair<String, String> {
        val (serviceId, next) = Bincode.readString(payload, 0)
        val (arg, _) = Bincode.readString(payload, next)
        return serviceId to arg
    }

    private fun readNotificationSpec(payload: ByteArray, offset: Int): NotificationSpec {
        var cursor = offset
        val (channelId, c1) = Bincode.readString(payload, cursor); cursor = c1
        val (notificationId, c2) = Bincode.readVarintI64(payload, cursor); cursor = c2
        val (title, c3) = Bincode.readString(payload, cursor); cursor = c3
        val (body, c4) = Bincode.readString(payload, cursor); cursor = c4
        val smallIcon = Bincode.readOption(payload, cursor) { p, o -> Bincode.readString(p, o) }
        cursor = smallIcon.consumed
        val ongoing = Bincode.readBool(payload, cursor); cursor = ongoing.consumed
        val fst = Bincode.readOption(payload, cursor) { p, o ->
            val (v, next) = Bincode.readVarintU64(p, o)
            Bincode.Decoded(v.toInt(), next)
        }
        return NotificationSpec(
            channelId = channelId,
            notificationId = notificationId.toInt(),
            title = title,
            body = body,
            smallIcon = smallIcon.value,
            ongoing = ongoing.value,
            foregroundServiceType = fst.value,
        )
    }

    // ---- Domain error encoders (matches Rust enum ServiceControlError) ----

    private fun encodeUnknownService(serviceId: String): ByteArray = ByteArrayOutputStream().apply {
        Bincode.writeEnumDiscriminant(this, 0)
        Bincode.writeString(this, serviceId)
    }.toByteArray()

    private fun encodeWakelockDenied(msg: String): ByteArray = ByteArrayOutputStream().apply {
        Bincode.writeEnumDiscriminant(this, 1)
        Bincode.writeString(this, msg)
    }.toByteArray()

    private fun encodePlatformError(msg: String): ByteArray = ByteArrayOutputStream().apply {
        Bincode.writeEnumDiscriminant(this, 2)
        Bincode.writeString(this, msg)
    }.toByteArray()

    private data class NotificationSpec(
        val channelId: String,
        val notificationId: Int,
        val title: String,
        val body: String,
        val smallIcon: String?,
        val ongoing: Boolean,
        val foregroundServiceType: Int?,
    )

    companion object {
        const val PLUGIN_ID: String = "istmo.service_control"
        private val EMPTY_OK = ByteArray(0)

        /**
         * ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC constant, spelled
         * out so the demo doesn't need `androidx.core` for a single value.
         */
        @Suppress("unused")
        const val FOREGROUND_SERVICE_TYPE_DATA_SYNC: Int = ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC
    }
}
