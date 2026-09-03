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
import java.io.ByteArrayOutputStream
import java.util.concurrent.atomic.AtomicInteger

/**
 * Kotlin backend for the `istmo.notifications` plugin.
 *
 * Wire methods:
 *  * `is_authorized() -> bool`
 *  * `request_authorization() -> bool`
 *  * `schedule(NotificationRequest) -> NotificationHandle`
 *  * `cancel(u32) -> ()`
 *
 * The plugin does not implement scheduling (delayed posts) via
 * `AlarmManager` yet — the value is honoured only for zero delay in this
 * demo. Adding a `BroadcastReceiver` + `PendingIntent` shim is the natural
 * follow-up.
 */
class NotificationsHandler(private val activity: Activity) : PluginHandler {

    companion object {
        const val PLUGIN_ID = "istmo.notifications"
    }

    private val nextId = AtomicInteger(1)
    private val manager = NotificationManagerCompat.from(activity)

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "is_authorized" -> encodeBool(manager.areNotificationsEnabled())
        "request_authorization" -> {
            // The runtime prompt for POST_NOTIFICATIONS is handled by the
            // Permissions plugin. This helper just reports the current
            // enabled state — same shape as iOS's `requestAuthorization`
            // which is idempotent for a second call.
            encodeBool(manager.areNotificationsEnabled())
        }
        "schedule" -> encodeHandle(schedule(decodeRequest(payload)))
        "cancel" -> {
            val (id, _) = decodeCancelId(payload)
            manager.cancel(id)
            ByteArray(0)
        }
        else -> error("unknown Notifications method: $method")
    }

    private fun schedule(request: Request): Int {
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
            throw PluginException(encodeError(0)) // PermissionDenied
        }
        return id
    }

    private fun ensureChannel(id: String, importance: Int) {
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

    private fun importanceToPriority(discriminant: Int): Int = when (discriminant) {
        0 -> NotificationCompat.PRIORITY_MIN
        1 -> NotificationCompat.PRIORITY_LOW
        2 -> NotificationCompat.PRIORITY_DEFAULT
        3 -> NotificationCompat.PRIORITY_HIGH
        else -> NotificationCompat.PRIORITY_DEFAULT
    }

    private fun channelImportance(discriminant: Int): Int = when (discriminant) {
        0 -> NotificationManager.IMPORTANCE_MIN
        1 -> NotificationManager.IMPORTANCE_LOW
        2 -> NotificationManager.IMPORTANCE_DEFAULT
        3 -> NotificationManager.IMPORTANCE_HIGH
        else -> NotificationManager.IMPORTANCE_DEFAULT
    }

    // ---- Bincode wire ----------------------------------------------------

    /**
     * Rust `NotificationRequest`:
     * ```rust
     * struct NotificationRequest {
     *     title: String,
     *     body: String,
     *     channel_id: String,
     *     importance: NotificationImportance,     // enum discriminant
     *     delay_seconds: Option<u32>,
     *     tag: Option<String>,
     * }
     * ```
     */
    private fun decodeRequest(payload: ByteArray): Request {
        var cursor = 0
        // Argument tuple `(request,)` — single element, no length prefix.
        val (title, c1) = Bincode.readString(payload, cursor); cursor = c1
        val (body, c2) = Bincode.readString(payload, cursor); cursor = c2
        val (channel, c3) = Bincode.readString(payload, cursor); cursor = c3
        val (importance, c4) = Bincode.readEnumDiscriminant(payload, cursor); cursor = c4
        // Option<u32> — u32 varint under bincode is a plain u64 varint bounded
        // to 32 bits.
        val (delay, c5) = Bincode.readOption(payload, cursor) { p, o ->
            val (v, next) = Bincode.readVarintU64(p, o)
            Bincode.Decoded(v.toInt(), next)
        }
        cursor = c5
        val (tag, c6) = Bincode.readOption(payload, cursor, Bincode::readString)
        cursor = c6
        return Request(title, body, channel, importance, delay, tag)
    }

    private fun decodeCancelId(payload: ByteArray): Pair<Int, Int> {
        val (v, next) = Bincode.readVarintU64(payload, 0)
        return v.toInt() to next
    }

    private fun encodeHandle(id: Int): ByteArray {
        val out = ByteArrayOutputStream(4)
        Bincode.writeVarintU64(out, id.toLong())
        return out.toByteArray()
    }

    private fun encodeBool(value: Boolean): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeBool(out, value)
        return out.toByteArray()
    }

    private fun encodeError(discriminant: Int): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeEnumDiscriminant(out, discriminant)
        return out.toByteArray()
    }

    private data class Request(
        val title: String,
        val body: String,
        val channelId: String,
        val importance: Int,
        val delaySeconds: Int?,
        val tag: String?,
    )
}
