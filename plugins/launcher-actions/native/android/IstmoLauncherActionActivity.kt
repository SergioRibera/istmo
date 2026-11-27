package dev.istmo.plugins.launcher_actions

import android.app.Activity
import android.content.Intent
import android.os.Bundle
import dev.istmo.runtime.IstmoRuntime

/**
 * Target of every launcher shortcut the plugin publishes, static
 * (`[[app.launcher_action]]`) and dynamic (`set_actions`).
 *
 * Publishes the shortcut's URL as a `DeepLink` on the `istmo.deeplinks`
 * queue (buffered until Rust subscribes), with `source =
 * "launcher-action:<id>"`, then brings up the app's launch activity and
 * finishes. Works whether or not the app handles `VIEW` intents itself.
 *
 * Not exported: the launcher starts shortcuts with the publishing app's
 * identity, so nothing outside the app can reach it.
 */
class IstmoLauncherActionActivity : Activity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val url = intent?.dataString
        if (url != null && savedInstanceState == null) {
            val id = intent?.getStringExtra(EXTRA_ACTION_ID)
            IstmoRuntime.publishEarlyQueue(
                LauncherActionLink.DEEPLINKS_CHANNEL,
                LauncherActionLink.DEEPLINKS_CAPACITY,
                LauncherActionLink.encode(
                    url,
                    id?.let { LauncherActionLink.SOURCE_PREFIX + it },
                    System.currentTimeMillis(),
                ),
            )
        }
        packageManager.getLaunchIntentForPackage(packageName)?.let { launch ->
            launch.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_REORDER_TO_FRONT)
            startActivity(launch)
        }
        finish()
    }

    companion object {
        /**
         * Intent extra carrying the action id; `istmo-build` writes the same
         * name into `res/xml/istmo_launcher_actions.xml`.
         */
        const val EXTRA_ACTION_ID = "dev.istmo.launcher_actions.ID"
    }
}
