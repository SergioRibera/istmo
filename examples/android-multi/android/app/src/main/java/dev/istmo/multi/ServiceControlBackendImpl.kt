package dev.istmo.multi

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.os.Build
import android.os.PowerManager
import androidx.core.app.NotificationCompat
import dev.istmo.multi.gen.NotificationSpec
import dev.istmo.multi.gen.ServiceControlBackend
import dev.istmo.multi.gen.ServiceControlError
import dev.istmo.multi.gen.WakelockToken
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoRuntime
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong

/**
 * Pure-Kotlin backend for `istmo.service_control`. Wire encode / decode
 * runs in the generated `ServiceControlDispatcher`; this class only speaks
 * platform APIs and returns typed values. Domain errors surface via
 * [BackendException] so the dispatcher can encode them into
 * `Frame::Respond { Err(bytes) }`.
 */
class ServiceControlBackendImpl(private val appContext: Context) : ServiceControlBackend {

    private val wakelocks = ConcurrentHashMap<ULong, PowerManager.WakeLock>()
    private val nextToken = AtomicLong(1L)

    override suspend fun start_foreground(service_id: String, spec: NotificationSpec) {
        val service = requireService(service_id)
        ensureChannel(spec.channelId)
        val notification = buildNotification(spec)
        val type = spec.foregroundServiceType?.toInt()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q && type != null) {
            service.startForeground(spec.notificationId, notification, type)
        } else {
            @Suppress("DEPRECATION")
            service.startForeground(spec.notificationId, notification)
        }
    }

    override suspend fun update_notification(service_id: String, spec: NotificationSpec) {
        requireService(service_id)
        ensureChannel(spec.channelId)
        val nm = appContext.getSystemService(NotificationManager::class.java)
        nm.notify(spec.notificationId, buildNotification(spec))
    }

    override suspend fun stop_foreground(service_id: String, remove_notification: Boolean) {
        val service = requireService(service_id)
        val flags = if (remove_notification) {
            Service.STOP_FOREGROUND_REMOVE
        } else {
            Service.STOP_FOREGROUND_DETACH
        }
        service.stopForeground(flags)
    }

    override suspend fun acquire_wakelock(service_id: String, tag: String): WakelockToken {
        requireService(service_id)
        val powerManager = appContext.getSystemService(PowerManager::class.java)
        val lock = powerManager.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "istmo:$tag")
        try {
            lock.acquire()
        } catch (t: SecurityException) {
            throw BackendException(
                ServiceControlError.WakelockDenied(t.message ?: "SecurityException"),
            )
        }
        val token = nextToken.getAndIncrement().toULong()
        wakelocks[token] = lock
        return WakelockToken(token)
    }

    override suspend fun release_wakelock(service_id: String, token: WakelockToken) {
        // service_id is not needed for release; the token identifies the
        // acquired wakelock across all services.
        wakelocks.remove(token.token)?.let { if (it.isHeld) it.release() }
    }

    override suspend fun stop_self(service_id: String) {
        requireService(service_id).stopSelf()
    }

    private fun requireService(serviceId: String): Service =
        IstmoRuntime.service(serviceId)
            ?: throw BackendException(ServiceControlError.UnknownService(serviceId))

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
}
