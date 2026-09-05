package dev.istmo.runtime

import android.app.Activity
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.os.Build
import androidx.core.app.NotificationCompat
import androidx.core.app.NotificationManagerCompat
import java.util.concurrent.atomic.AtomicInteger

/**
 * Android impl of the codegen `NotificationsBackend` interface.
 *
 * `NotificationManagerCompat` fronts the OS surface; channels are
 * lazily created on first use per `channelId` so callers do not have to
 * pre-register them.
 *
 * Delayed posts are not implemented — a real scheduler needs an
 * `AlarmManager` + `BroadcastReceiver` shim; this demo honours only
 * zero-delay requests.
 */
class NotificationsBackendImpl(private val activity: Activity) : NotificationsBackend {

    private val nextId = AtomicInteger(1)
    private val manager = NotificationManagerCompat.from(activity)

    override suspend fun is_authorized(): Boolean = manager.areNotificationsEnabled()

    override suspend fun request_authorization(): Boolean {
        // The runtime prompt for POST_NOTIFICATIONS is handled by the
        // Permissions plugin. This helper just reports the current
        // enabled state — same shape as iOS's `requestAuthorization`
        // which is idempotent for a second call.
        return manager.areNotificationsEnabled()
    }

    override suspend fun schedule(request: NotificationRequest): NotificationHandle {
        ensureChannel(request.channelId, request.importance)
        val id = nextId.getAndIncrement()
        val intent = Intent(activity, activity.javaClass).apply {
            flags = Intent.FLAG_ACTIVITY_SINGLE_TOP or Intent.FLAG_ACTIVITY_CLEAR_TOP
        }
        val pending = PendingIntent.getActivity(
            activity,
            id,
            intent,
            PendingIntent.FLAG_UPDATE_CURRENT or PendingIntent.FLAG_IMMUTABLE,
        )
        val notif = NotificationCompat.Builder(activity, request.channelId)
            .setSmallIcon(android.R.drawable.ic_dialog_info)
            .setContentTitle(request.title)
            .setContentText(request.body)
            .setPriority(importanceToPriority(request.importance))
            .setContentIntent(pending)
            .setAutoCancel(true)
            .build()
        if (manager.areNotificationsEnabled()) {
            manager.notify(request.tag, id, notif)
        } else {
            throw BackendException(NotificationError.PermissionDenied)
        }
        return NotificationHandle(id.toUInt())
    }

    override suspend fun cancel(id: UInt) {
        manager.cancel(id.toInt())
    }

    // ---- Channels + importance mapping ----------------------------------

    private fun ensureChannel(id: String, importance: NotificationImportance) {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.O) return
        val nm = activity.getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        if (nm.getNotificationChannel(id) != null) return
        val channel = NotificationChannel(
            id,
            id.replaceFirstChar { it.titlecase() },
            channelImportance(importance),
        )
        nm.createNotificationChannel(channel)
    }

    private fun importanceToPriority(importance: NotificationImportance): Int = when (importance) {
        NotificationImportance.Min -> NotificationCompat.PRIORITY_MIN
        NotificationImportance.Low -> NotificationCompat.PRIORITY_LOW
        NotificationImportance.Default -> NotificationCompat.PRIORITY_DEFAULT
        NotificationImportance.High -> NotificationCompat.PRIORITY_HIGH
    }

    private fun channelImportance(importance: NotificationImportance): Int = when (importance) {
        NotificationImportance.Min -> NotificationManager.IMPORTANCE_MIN
        NotificationImportance.Low -> NotificationManager.IMPORTANCE_LOW
        NotificationImportance.Default -> NotificationManager.IMPORTANCE_DEFAULT
        NotificationImportance.High -> NotificationManager.IMPORTANCE_HIGH
    }
}
