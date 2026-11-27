package dev.istmo.plugins.notifications

import android.app.AlarmManager
import android.content.Context
import android.os.Build

/**
 * The one `AlarmManager` alarm of the schedule, for its soonest entry.
 *
 * Exact (`setExactAndAllowWhileIdle`) when the app may schedule exact
 * alarms (always below API 31; with `SCHEDULE_EXACT_ALARM` granted or
 * `USE_EXACT_ALARM` declared from 31, the former denied by default from
 * 34), else inexact `setAndAllowWhileIdle`. Alarms die with a reboot or
 * an app update: [IstmoNotificationBootReceiver] arms it again.
 */
internal object Alarms {
    fun canExact(context: Context): Boolean {
        if (Build.VERSION.SDK_INT < 31) return true
        val alarms = context.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        return alarms.canScheduleExactAlarms()
    }

    /** Arms the alarm at [atMs] (wall clock), or cancels it for `null`; whether it is exact. */
    fun arm(context: Context, atMs: Long?): Boolean {
        val alarms = context.getSystemService(Context.ALARM_SERVICE) as AlarmManager
        if (atMs == null) {
            Intents.alarm(context, create = false)?.let { alarms.cancel(it) }
            return canExact(context)
        }
        val intent = Intents.alarm(context, create = true) ?: return false
        val exact = canExact(context)
        try {
            when {
                exact && Build.VERSION.SDK_INT >= 23 -> alarms.setExactAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, atMs, intent)
                exact -> alarms.setExact(AlarmManager.RTC_WAKEUP, atMs, intent)
                Build.VERSION.SDK_INT >= 23 -> alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, atMs, intent)
                else -> alarms.set(AlarmManager.RTC_WAKEUP, atMs, intent)
            }
        } catch (_: SecurityException) {
            // The exact grant was revoked between the check and the call.
            if (Build.VERSION.SDK_INT >= 23) {
                alarms.setAndAllowWhileIdle(AlarmManager.RTC_WAKEUP, atMs, intent)
            } else {
                alarms.set(AlarmManager.RTC_WAKEUP, atMs, intent)
            }
            return false
        }
        return exact
    }
}
