package dev.istmo.plugins.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/** The schedule alarm: shows what is due and arms the next one. No Rust involved. */
class IstmoNotificationAlarmReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != Intents.ACTION_ALARM) return
        val pending = goAsync()
        Thread({
            try {
                NotificationCenter.init(context)
                NotificationCenter.fireDue()
            } catch (e: Exception) {
                android.util.Log.w("istmo.notifications", "schedule work failed", e)
            } finally {
                pending.finish()
            }
        }, "istmo-notifications-alarm").start()
    }
}
