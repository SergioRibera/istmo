package dev.istmo.plugins.notifications

import android.app.Activity
import android.app.Application
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import androidx.activity.ComponentActivity
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.core.app.ActivityCompat
import dev.istmo.runtime.IstmoHost
import kotlin.coroutines.resume
import kotlinx.coroutines.suspendCancellableCoroutine

/**
 * One runtime permission prompt on the current istmo activity.
 *
 * - A `ComponentActivity` host: `ActivityResultRegistry` under a fixed
 *   key; a host recreated while the prompt shows gets the launcher
 *   registered again under the same key, so the result reaches it.
 * - Any other host (`NativeActivity`): `ActivityCompat.requestPermissions`,
 *   whose result goes to the activity; the prompt counts as answered when
 *   the activity resumes after the dialog paused it (or when nothing
 *   paused it within a second: no dialog was shown).
 *
 * The caller reads the permission state afterwards.
 */
internal class PermissionPrompt(private val app: Application) {
    private val main = Handler(Looper.getMainLooper())

    /** Returns once the person answered, or at once when there is no activity. */
    suspend fun request(permission: String) {
        suspendCancellableCoroutine { cont ->
            main.post {
                val host = IstmoHost.currentActivity()
                if (host == null) {
                    cont.resume(Unit)
                    return@post
                }
                val session = Session(permission) { if (cont.isActive) cont.resume(Unit) }
                cont.invokeOnCancellation { main.post(session::end) }
                session.start(host)
            }
        }
    }

    private inner class Session(val permission: String, val done: () -> Unit) : Application.ActivityLifecycleCallbacks {
        private var launcher: ActivityResultLauncher<String>? = null
        private var paused = false
        private var watched: Activity? = null
        private var ended = false

        fun start(host: Activity) {
            app.registerActivityLifecycleCallbacks(this)
            if (host is ComponentActivity) {
                register(host).launch(permission)
            } else {
                watched = host
                ActivityCompat.requestPermissions(host, arrayOf(permission), REQUEST_CODE)
                main.postDelayed({ if (!paused) finish() }, NO_DIALOG_MS)
            }
        }

        private fun register(host: ComponentActivity): ActivityResultLauncher<String> {
            launcher?.unregister()
            val registered = host.activityResultRegistry.register(KEY, ActivityResultContracts.RequestPermission()) { finish() }
            launcher = registered
            return registered
        }

        private fun finish() {
            if (ended) return
            end()
            done()
        }

        fun end() {
            if (ended) return
            ended = true
            launcher?.unregister()
            launcher = null
            watched = null
            app.unregisterActivityLifecycleCallbacks(this)
        }

        override fun onActivityStarted(activity: Activity) {
            // A recreated ComponentActivity: the result is delivered to the launcher of the same key.
            if (launcher != null && activity is ComponentActivity) register(activity)
        }

        override fun onActivityPaused(activity: Activity) {
            if (activity === watched) paused = true
        }

        override fun onActivityResumed(activity: Activity) {
            if (activity === watched && paused) finish()
        }

        override fun onActivityCreated(activity: Activity, savedInstanceState: Bundle?) {}
        override fun onActivityStopped(activity: Activity) {}
        override fun onActivitySaveInstanceState(activity: Activity, outState: Bundle) {}
        override fun onActivityDestroyed(activity: Activity) {}
    }

    private companion object {
        const val KEY = "istmo.notifications.permission"
        const val REQUEST_CODE = 0x1570
        const val NO_DIALOG_MS = 1_000L
    }
}
