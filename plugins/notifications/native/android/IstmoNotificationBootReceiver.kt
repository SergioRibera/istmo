package dev.istmo.plugins.notifications

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent

/**
 * Re-arms the schedule after what kills or shifts alarms: boot
 * (`BOOT_COMPLETED`), clock and zone changes (`TIME_SET`,
 * `TIMEZONE_CHANGED`: future deliveries are computed again, so
 * local-time rules follow the new zone), an app update
 * (`MY_PACKAGE_REPLACED`) and the exact-alarm grant changing
 * (`SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED`). Entries missed
 * meanwhile go through their `MissedPolicy`.
 *
 * Exported for the system broadcasts, which other apps cannot send; it
 * reads nothing from the intent but its action.
 */
class IstmoNotificationBootReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        val clockChanged = when (intent.action) {
            Intent.ACTION_TIME_CHANGED, Intent.ACTION_TIMEZONE_CHANGED -> true
            Intent.ACTION_BOOT_COMPLETED, Intent.ACTION_MY_PACKAGE_REPLACED, EXACT_ALARM_CHANGED -> false
            else -> return
        }
        val pending = goAsync()
        Thread({
            try {
                NotificationCenter.init(context)
                NotificationCenter.rearm(clockChanged)
            } finally {
                pending.finish()
            }
        }, "istmo-notifications-rearm").start()
    }

    private companion object {
        /** `AlarmManager.ACTION_SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED` (API 31). */
        const val EXACT_ALARM_CHANGED = "android.app.action.SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED"
    }
}
