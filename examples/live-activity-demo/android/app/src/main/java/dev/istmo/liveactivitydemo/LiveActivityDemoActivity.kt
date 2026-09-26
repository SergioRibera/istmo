package dev.istmo.liveactivitydemo

import android.app.NativeActivity
import android.os.Bundle
import dev.istmo.plugins.liveactivity.LiveActivityBackendImpl
import dev.istmo.runtime.IstmoHost
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.LiveActivityCodecsImpl
import dev.istmo.runtime.LiveActivityDispatcher

class LiveActivityDemoActivity : NativeActivity() {

    private lateinit var liveActivityBackend: LiveActivityBackendImpl

    override fun onCreate(savedInstanceState: Bundle?) {
        // Starts the runtime. istmo.toml turns off auto-registration for
        // istmo.live_activity: the backend below carries the demo's handler.
        IstmoHost.onCreate(this)

        liveActivityBackend = LiveActivityBackendImpl(applicationContext).apply {
            register(TimerLiveActivityHandler(this@LiveActivityDemoActivity))
        }
        IstmoRuntime.registerHandler(
            LiveActivityDispatcher.PLUGIN_ID,
            LiveActivityDispatcher(liveActivityBackend, LiveActivityCodecsImpl()),
        )

        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoHost.onDestroy(this)
    }
}

