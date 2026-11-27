package dev.istmo.plugins.notifications

import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.net.Uri
import android.os.Build

/**
 * Intents and `PendingIntent`s of the plugin's notifications.
 *
 * Every `PendingIntent` is `FLAG_IMMUTABLE` except the inline reply one
 * (`FLAG_MUTABLE`, always an explicit component: API 34 refuses implicit
 * mutable ones). Taps and actions that open the app go straight to
 * [IstmoNotificationActivity] with `getActivity` (no trampoline through a
 * receiver, which API 31 forbids); background actions, snoozes and
 * dismissals go to [IstmoNotificationReceiver].
 *
 * Each intent carries the encoded `NotificationResponse` template of its
 * target ([EXTRA_RESPONSE]); the receiving side fills `reply`,
 * `launchedApp` and `ack`. The data URI `istmo-notification://<id>/<key>`
 * keeps the intents of different notifications and actions apart.
 */
internal object Intents {
    const val ACTION_OPEN = "dev.istmo.notifications.OPEN"
    const val ACTION_BACKGROUND = "dev.istmo.notifications.BACKGROUND"
    const val ACTION_DISMISS = "dev.istmo.notifications.DISMISS"
    const val ACTION_SNOOZE = "dev.istmo.notifications.SNOOZE"
    const val ACTION_ALARM = "dev.istmo.notifications.ALARM"

    const val EXTRA_ID = "dev.istmo.notifications.ID"
    const val EXTRA_RESPONSE = "dev.istmo.notifications.RESPONSE"
    const val EXTRA_SNOOZE_MS = "dev.istmo.notifications.SNOOZE_MS"

    /** Set when the target should cancel the notification (keep_on_tap off). */
    const val EXTRA_CANCEL = "dev.istmo.notifications.CANCEL"

    /** Set on reply actions: the receiving side re-posts to clear the spinner. */
    const val EXTRA_REPLY = "dev.istmo.notifications.IS_REPLY"

    /** `RemoteInput` result key of replies and native choices. */
    const val REMOTE_INPUT_KEY = "dev.istmo.notifications.REPLY"

    private const val SCHEME = "istmo-notification"

    private fun data(id: String, key: String): Uri =
        Uri.Builder().scheme(SCHEME).authority(Uri.encode(id)).appendPath(key).build()

    private fun flags(mutable: Boolean): Int {
        var flags = PendingIntent.FLAG_UPDATE_CURRENT
        flags = flags or if (mutable && Build.VERSION.SDK_INT >= 31) {
            PendingIntent.FLAG_MUTABLE
        } else if (!mutable && Build.VERSION.SDK_INT >= 23) {
            PendingIntent.FLAG_IMMUTABLE
        } else {
            0
        }
        return flags
    }

    /** Opens the app through the trampoline activity with [response]. */
    fun activity(context: Context, id: String, key: String, response: ByteArray, cancel: Boolean, mutable: Boolean): PendingIntent {
        val intent = Intent(context, IstmoNotificationActivity::class.java)
            .setAction(ACTION_OPEN)
            .setData(data(id, key))
            .putExtra(EXTRA_ID, id)
            .putExtra(EXTRA_RESPONSE, response)
            .putExtra(EXTRA_CANCEL, cancel)
            .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION)
        return PendingIntent.getActivity(context, 0, intent, flags(mutable))
    }

    /** A background action (no UI) with [response]. */
    fun background(context: Context, id: String, key: String, response: ByteArray, reply: Boolean, mutable: Boolean): PendingIntent {
        val intent = Intent(context, IstmoNotificationReceiver::class.java)
            .setAction(ACTION_BACKGROUND)
            .setData(data(id, key))
            .putExtra(EXTRA_ID, id)
            .putExtra(EXTRA_RESPONSE, response)
            .putExtra(EXTRA_REPLY, reply)
        return PendingIntent.getBroadcast(context, 0, intent, flags(mutable))
    }

    fun snooze(context: Context, id: String, key: String, delayMs: Long): PendingIntent {
        val intent = Intent(context, IstmoNotificationReceiver::class.java)
            .setAction(ACTION_SNOOZE)
            .setData(data(id, key))
            .putExtra(EXTRA_ID, id)
            .putExtra(EXTRA_SNOOZE_MS, delayMs)
        return PendingIntent.getBroadcast(context, 0, intent, flags(false))
    }

    fun dismiss(context: Context, id: String): PendingIntent {
        val intent = Intent(context, IstmoNotificationReceiver::class.java)
            .setAction(ACTION_DISMISS)
            .setData(data(id, "istmo.dismiss"))
            .putExtra(EXTRA_ID, id)
        return PendingIntent.getBroadcast(context, 0, intent, flags(false))
    }

    /** The one schedule alarm. */
    fun alarm(context: Context, create: Boolean): PendingIntent? {
        val intent = Intent(context, IstmoNotificationAlarmReceiver::class.java).setAction(ACTION_ALARM)
        val flags = flags(false) or if (create) 0 else PendingIntent.FLAG_NO_CREATE
        return PendingIntent.getBroadcast(context, 0, intent, flags)
    }
}
