package dev.istmo.liveactivitydemo

import android.app.NativeActivity
import android.os.Bundle
import dev.istmo.plugins.liveactivity.LiveActivityBackendImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.LiveActivityCodecsImpl
import dev.istmo.runtime.LiveActivityDispatcher

/**
 * NativeActivity subclass that boots the istmo runtime and registers
 * the `LiveActivity` dispatcher before the native activity's onCreate
 * runs `android_main`.
 *
 * `<meta-data android:name="android.app.lib_name"
 * android:value="live_activity_demo"/>` in the manifest tells
 * NativeActivity which cdylib to load. `IstmoRuntime.start()` then:
 *
 *  1. Loads the same library via `System.loadLibrary` (idempotent).
 *  2. Runs `nativeStart` — invokes `__istmo_configure_runtime` emitted
 *     by `istmo::runtime!` in the Rust cdylib, initialising the process
 *     `Runtime`.
 *  3. Spawns the pump thread that drains outbound frames.
 */
class LiveActivityDemoActivity : NativeActivity() {

    private lateinit var liveActivityBackend: LiveActivityBackendImpl

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        check(runtime.start()) { "IstmoRuntime.start() failed — pump did not initialise" }

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
