package dev.istmo.biometricdemo

import android.os.Bundle
import com.google.androidgamesdk.GameActivity
import dev.istmo.plugins.biometric.BiometricBackendImpl
import dev.istmo.runtime.BiometricCodecsImpl
import dev.istmo.runtime.BiometricDispatcher
import dev.istmo.runtime.IstmoRuntime

/**
 * `GameActivity` is an `AppCompatActivity`, so it can host the
 * `BiometricPrompt` fragment. The dispatcher is registered before
 * `super.onCreate`, which boots the Rust side (`android_main`) that
 * acquires `BiometricClient` right away.
 */
class BiometricDemoActivity : GameActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        check(IstmoRuntime.start("biometric_demo")) { "IstmoRuntime.start() failed" }
        IstmoRuntime.registerHandler(
            BiometricDispatcher.PLUGIN_ID,
            BiometricDispatcher(BiometricBackendImpl(this), BiometricCodecsImpl()),
        )
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
