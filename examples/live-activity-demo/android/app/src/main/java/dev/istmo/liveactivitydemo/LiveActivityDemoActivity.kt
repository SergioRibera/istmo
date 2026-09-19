package dev.istmo.liveactivitydemo

import android.app.NativeActivity
import android.os.Bundle
import dev.istmo.plugins.liveactivity.LiveActivityBackendImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.LiveActivityCodecsImpl
import dev.istmo.runtime.LiveActivityDispatcher

class LiveActivityDemoActivity : NativeActivity() {

    private lateinit var liveActivityBackend: LiveActivityBackendImpl

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        check(runtime.start("live_activity_demo")) {
            "IstmoRuntime.start() failed — pump did not initialise"
        }

        liveActivityBackend = LiveActivityBackendImpl(applicationContext).apply {
            register(TimerLiveActivityHandler(this@LiveActivityDemoActivity))
        }
        runtime.registerHandler(
            LiveActivityDispatcher.PLUGIN_ID,
            LiveActivityDispatcher(liveActivityBackend, LiveActivityCodecsImpl()),
        )

        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}

