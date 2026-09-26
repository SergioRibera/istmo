package dev.istmo.runtime

import android.app.NativeActivity
import android.os.Bundle
import com.google.androidgamesdk.GameActivity

/**
 * `GameActivity` host for Rust-driven apps (winit / eframe with the
 * `android-game-activity` feature). Starts the runtime and registers
 * every plugin before the Rust entry point runs; being a
 * `ComponentActivity`, it can host plugins that register activity-result
 * launchers.
 *
 * Declare it (or a subclass) in `AndroidManifest.xml` with the
 * `android.app.lib_name` meta-data. Apps using it add the
 * `androidx.games:games-activity` dependency.
 */
open class IstmoGameActivity : GameActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        IstmoHost.onCreate(this)
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoHost.onDestroy(this)
    }
}

/**
 * `NativeActivity` host for Rust-driven apps (winit / eframe with the
 * `android-native-activity` feature). Plugins whose backend needs a
 * `ComponentActivity` cannot run on it; use [IstmoGameActivity] for those.
 */
open class IstmoNativeActivity : NativeActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        IstmoHost.onCreate(this)
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoHost.onDestroy(this)
    }
}
