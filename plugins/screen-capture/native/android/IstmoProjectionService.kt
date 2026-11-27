package dev.istmo.plugins.screencapture

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.graphics.drawable.Icon
import android.os.Build
import android.os.Handler
import android.os.IBinder
import android.os.Looper
import android.util.Log
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout

/**
 * The `mediaProjection` foreground service a projection runs under
 * (required from API 29, and on API 34+ started only after the person
 * consented). Not exported; [ProjectionForeground] starts and stops it.
 *
 * Its notification carries a Stop action that ends every projection the
 * process holds, as the system's own stop (status-bar chip, lock screen)
 * does: sessions end with `ConsentRevoked`.
 */
class IstmoProjectionService : Service() {

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        if (intent?.action == ACTION_STOP) {
            ProjectionForeground.stopRequested()
            return START_NOT_STICKY
        }
        try {
            val notification = notification()
            if (Build.VERSION.SDK_INT >= Api.Q) {
                startForeground(NOTIFICATION_ID, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION)
            } else {
                startForeground(NOTIFICATION_ID, notification)
            }
            ProjectionForeground.onForeground(this, null)
        } catch (e: RuntimeException) {
            // `SecurityException` without the consent or the
            // FOREGROUND_SERVICE_MEDIA_PROJECTION permission,
            // `ForegroundServiceStartNotAllowedException` from the background.
            Log.w(TAG, "could not enter the foreground", e)
            ProjectionForeground.onForeground(this, e)
            stopSelf()
        }
        // A projection does not survive the process: never restart.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        ProjectionForeground.onDestroyed(this)
        super.onDestroy()
    }

    private fun notification(): Notification {
        val label = applicationInfo.loadLabel(packageManager)
        val icon = applicationInfo.icon.takeIf { it != 0 } ?: android.R.drawable.ic_menu_camera
        val builder = if (Build.VERSION.SDK_INT >= Api.O) {
            val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, CHANNEL_NAME, NotificationManager.IMPORTANCE_LOW),
            )
            Notification.Builder(this, CHANNEL_ID)
        } else {
            @Suppress("DEPRECATION")
            Notification.Builder(this)
        }
        builder
            .setSmallIcon(icon)
            .setContentTitle(label)
            .setContentText(CONTENT_TEXT)
            .setOngoing(true)
        if (Build.VERSION.SDK_INT >= Api.M) {
            val stop = PendingIntent.getService(
                this,
                0,
                Intent(this, IstmoProjectionService::class.java).setAction(ACTION_STOP),
                PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
            )
            builder.addAction(
                Notification.Action.Builder(Icon.createWithResource(this, icon), STOP_LABEL, stop).build(),
            )
        }
        return builder.build()
    }

    companion object {
        private const val TAG = "istmo.screen_capture"
        const val ACTION_STOP = "dev.istmo.plugins.screencapture.STOP"
        private const val NOTIFICATION_ID = 0x15_70_5c
        private const val CHANNEL_ID = "istmo.screen_capture"
        private const val CHANNEL_NAME = "Screen capture"
        private const val CONTENT_TEXT = "Capturing the screen"
        private const val STOP_LABEL = "Stop"
    }
}

/**
 * Process-scope control of [IstmoProjectionService]: every projection
 * holds one lease, the service runs in the foreground while any lease is
 * out and stops with the last one.
 *
 * Every start / stop decision runs on the main thread (where the service
 * callbacks run too), so a lease taken while the last one is given back
 * never sees its service stopped under it. A service started but not yet
 * in the foreground is never stopped from outside (that crashes the
 * process): it stops itself once it got there, unless a lease wants it
 * again by then.
 */
internal object ProjectionForeground {
    private const val START_TIMEOUT_MS = 5_000L

    private val main = Handler(Looper.getMainLooper())

    // Main thread only.
    private var leases = 0
    private var service: IstmoProjectionService? = null
    private var waiting: CompletableDeferred<Unit>? = null
    private var stopWhenUp = false

    /** Called when the notification's Stop action fires. */
    @Volatile
    var onStopRequested: (() -> Unit)? = null

    /** Take a lease, starting the service and waiting until it is in the foreground. */
    suspend fun acquire(context: Context) {
        // Not cancellable: the lease taken on the main thread must come
        // back to a caller that gives it back.
        val deferred = withContext(NonCancellable + Dispatchers.Main) {
            leases += 1
            stopWhenUp = false
            if (service != null) return@withContext null
            waiting ?: CompletableDeferred<Unit>().also { started ->
                try {
                    val intent = Intent(context, IstmoProjectionService::class.java)
                    if (Build.VERSION.SDK_INT >= Api.O) context.startForegroundService(intent) else context.startService(intent)
                } catch (e: RuntimeException) {
                    leases -= 1
                    throw e
                }
                waiting = started
            }
        } ?: return
        try {
            withTimeout(START_TIMEOUT_MS) { deferred.await() }
        } catch (e: Throwable) {
            release(context)
            throw e
        }
    }

    /** Give a lease back; the last one stops the service. Any thread. */
    fun release(context: Context) {
        main.post {
            if (leases == 0) return@post
            leases -= 1
            if (leases > 0) return@post
            val running = service
            service = null
            if (running != null) {
                context.stopService(Intent(context, IstmoProjectionService::class.java))
            } else if (waiting != null) {
                stopWhenUp = true
            }
        }
    }

    /** Main thread (`onStartCommand`). */
    fun onForeground(running: IstmoProjectionService, error: Throwable?) {
        val deferred = waiting
        waiting = null
        if (error != null) {
            stopWhenUp = false
            deferred?.completeExceptionally(error)
            return
        }
        if (stopWhenUp || leases == 0) {
            // Every lease went while it was starting.
            stopWhenUp = false
            running.stopSelf()
            deferred?.completeExceptionally(IllegalStateException("projection service no longer needed"))
            return
        }
        service = running
        deferred?.complete(Unit)
    }

    /** Main thread (`onDestroy`). */
    fun onDestroyed(stopped: IstmoProjectionService) {
        if (service === stopped) service = null
    }

    fun stopRequested() {
        onStopRequested?.invoke()
    }
}
