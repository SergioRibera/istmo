// Hand-mirrored equivalent of `istmo_build::generate_android_service` output
// for the `dev.istmo.multi.sync` plugin. Kept in-repo (rather than emitted
// from build.rs) so the demo does not depend on a working build integration.

package dev.istmo.multi

import android.content.Intent
import androidx.lifecycle.LifecycleService
import androidx.lifecycle.lifecycleScope
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.launch

class SyncService : LifecycleService() {

    companion object {
        const val PLUGIN_ID: String = "dev.istmo.multi.sync"
        private const val LIBRARY_NAME: String = "istmo_android_multi_app"

        init {
            System.loadLibrary(LIBRARY_NAME)
        }
    }

    override fun onCreate() {
        super.onCreate()
        IstmoRuntime.start()
        IstmoRuntime.registerService(PLUGIN_ID, this)
    }

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        super.onStartCommand(intent, flags, startId)
        val payload = Bincode.writeString(PLUGIN_ID)
        lifecycleScope.launch {
            IstmoRuntime.call(PLUGIN_ID, "on_start", payload)
        }
        return START_STICKY
    }

    override fun onDestroy() {
        val payload = ByteArray(0)
        lifecycleScope.launch {
            IstmoRuntime.call(PLUGIN_ID, "on_stop", payload)
        }
        IstmoRuntime.unregisterService(PLUGIN_ID)
        super.onDestroy()
    }
}
