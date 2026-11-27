package dev.istmo.plugins.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.util.Log
import androidx.core.app.RemoteInput

/**
 * Background actions (`opensApp = false`), snoozes and dismissals.
 *
 * - Action / background reply: the response goes on the lane with an
 *   `AckToken`; the receiver stays alive (`goAsync`) until the app calls
 *   `complete(ack)` or 9 s pass. When no runtime runs in this process
 *   (the button woke a cold process; app code only runs with an
 *   activity), the response is queued in a file, delivered at the next
 *   start, without an ack, and the receiver finishes at once.
 * - Snooze: removed and scheduled again, then a `Snoozed` event.
 * - Dismiss (delete intent): a `Dismissed` event, later regular updates
 *   are dropped.
 *
 * Not exported: only the plugin's `PendingIntent`s reach it.
 */
class IstmoNotificationReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val id = intent.getStringExtra(Intents.EXTRA_ID) ?: return
        val pending = goAsync()
        Thread({
            var handedOver = false
            try {
                NotificationCenter.init(context)
                when (intent.action) {
                    Intents.ACTION_DISMISS -> NotificationCenter.dismissedByUser(id)
                    Intents.ACTION_SNOOZE -> NotificationCenter.snooze(id, intent.getLongExtra(Intents.EXTRA_SNOOZE_MS, 0))
                    Intents.ACTION_BACKGROUND -> handedOver = background(context, intent, id, pending)
                }
            } catch (e: RuntimeException) {
                Log.w(TAG, "notification action failed", e)
            } finally {
                if (!handedOver) pending.finish()
            }
        }, "istmo-notifications-action").start()
    }

    /** Whether [pending] now belongs to an ack. */
    private fun background(context: Context, intent: Intent, id: String, pending: PendingResult): Boolean {
        val bytes = intent.getByteArrayExtra(Intents.EXTRA_RESPONSE) ?: return false
        val template = try {
            Responses.decode(bytes)
        } catch (_: IllegalArgumentException) {
            return false
        }
        val isReply = intent.getBooleanExtra(Intents.EXTRA_REPLY, false)
        val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(Intents.REMOTE_INPUT_KEY)?.toString()
        val response = template.copy(reply = reply ?: template.reply)
        if (!NotificationCenter.hostAttached) {
            Responses.enqueue(context, response)
            if (isReply) NotificationCenter.repostAfterReply(id)
            return false
        }
        val ack = NotificationCenter.holdAck(id, pending, isReply)
        Responses.publish(response.copy(ack = ack))
        return true
    }

    private companion object {
        const val TAG = "istmo.notifications"
    }
}
