@file:Suppress("UNUSED_PARAMETER", "unused")

package android.app

open class Service : android.content.Context() {
    open fun onCreate() {}
    open fun onStartCommand(intent: android.content.Intent?, flags: Int, startId: Int): Int = 0
    open fun onBind(intent: android.content.Intent?): android.os.IBinder? = null
    open fun onDestroy() {}
    fun startForeground(id: Int, notification: Notification) {}
    fun startForeground(id: Int, notification: Notification, foregroundServiceType: Int) {}
    fun stopSelf() {}

    companion object {
        const val START_STICKY: Int = 1
        const val START_NOT_STICKY: Int = 2
    }
}

open class Activity : android.content.Context() {
    val isFinishing: Boolean get() = false
    val isDestroyed: Boolean get() = false
    fun shouldShowRequestPermissionRationale(permission: String): Boolean = false

    companion object {
        const val RESULT_OK: Int = -1
    }
}

class Notification {
    class Builder(context: android.content.Context, channelId: String) {
        constructor(context: android.content.Context) : this(context, "")

        fun setSmallIcon(icon: Int): Builder = this
        fun setContentTitle(title: CharSequence?): Builder = this
        fun setContentText(text: CharSequence?): Builder = this
        fun setOngoing(ongoing: Boolean): Builder = this
        fun addAction(action: Action): Builder = this
        fun build(): Notification = Notification()
    }

    class Action {
        class Builder(icon: android.graphics.drawable.Icon?, title: CharSequence?, intent: PendingIntent?) {
            fun build(): Action = Action()
        }
    }
}

class NotificationChannel(id: String, name: CharSequence, importance: Int)

open class NotificationManager {
    fun createNotificationChannel(channel: NotificationChannel) {}
    fun areNotificationsEnabled(): Boolean = true

    companion object {
        const val IMPORTANCE_LOW: Int = 2
    }
}

class PendingIntent {
    companion object {
        const val FLAG_IMMUTABLE: Int = 67108864
        const val FLAG_UPDATE_CURRENT: Int = 134217728

        @JvmStatic
        fun getService(context: android.content.Context, requestCode: Int, intent: android.content.Intent, flags: Int): PendingIntent =
            PendingIntent()
    }
}
