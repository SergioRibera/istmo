package dev.istmo.datastoredemo

import android.app.NativeActivity
import android.os.Bundle
import dev.istmo.runtime.DataStoreCodecsImpl
import dev.istmo.runtime.DataStoreDispatcher
import dev.istmo.runtime.DataStoreFactoryImpl
import dev.istmo.runtime.IstmoRuntime

/**
 * NativeActivity subclass that boots the istmo runtime and registers the
 * `DataStore` dispatcher before the native activity's onCreate runs
 * `android_main`.
 *
 * `<meta-data android:name="android.app.lib_name" android:value="data_store_demo"/>`
 * in the manifest tells NativeActivity which cdylib to load. We separately
 * call `IstmoRuntime.start()`, which:
 *
 *  1. Loads the same library via `System.loadLibrary` (idempotent).
 *  2. Runs `nativeStart` — invokes `__istmo_configure_runtime` emitted by
 *     `istmo::runtime!` in the Rust cdylib, initialising the process
 *     `Runtime`.
 *  3. Spawns the pump thread that drains outbound frames.
 */
class DataStoreDemoActivity : NativeActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        val ok = runtime.start()
        check(ok) { "IstmoRuntime.start() failed — pump did not initialise" }

        runtime.registerHandler(
            DataStoreDispatcher.PLUGIN_ID,
            DataStoreDispatcher(DataStoreFactoryImpl(this), DataStoreCodecsImpl()),
        )

        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
