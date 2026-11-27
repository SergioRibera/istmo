package dev.istmo.plugins.notifications

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import androidx.core.app.RemoteInput

/**
 * Target of every tap and of the actions that open the app.
 *
 * Publishes the `NotificationResponse` (with the reply text of an inline
 * reply, `launchedApp` when no runtime ran in this process yet) on the
 * responses lane, and its deep link when it has a URL. Both are buffered
 * by [dev.istmo.runtime.IstmoRuntime.publishEarlyQueue] until the runtime
 * starts, which the app's launch activity, brought up right after, does.
 *
 * Not exported; started with `getActivity` (Android 12 forbids starting
 * activities from a notification through a receiver).
 */
class IstmoNotificationActivity : Activity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        if (savedInstanceState == null) handle(intent)
        packageManager.getLaunchIntentForPackage(packageName)?.let { launch ->
            launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
            startActivity(launch)
        }
        finish()
    }

    private fun handle(intent: Intent?) {
        intent ?: return
        val id = intent.getStringExtra(Intents.EXTRA_ID) ?: return
        val bytes = intent.getByteArrayExtra(Intents.EXTRA_RESPONSE) ?: return
        val template = try {
            Responses.decode(bytes)
        } catch (_: IllegalArgumentException) {
            return
        }
        val reply = RemoteInput.getResultsFromIntent(intent)?.getCharSequence(Intents.REMOTE_INPUT_KEY)?.toString()
        Responses.publish(template.copy(reply = reply ?: template.reply, launchedApp = !NotificationCenter.hostAttached))
        if (intent.getBooleanExtra(Intents.EXTRA_CANCEL, false)) {
            NotificationCenter.init(this)
            NotificationCenter.removeShown(id)
        }
    }
}
