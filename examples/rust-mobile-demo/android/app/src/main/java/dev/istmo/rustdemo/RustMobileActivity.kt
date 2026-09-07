package dev.istmo.rustdemo

import android.app.NativeActivity
import android.content.Intent
import android.os.Bundle
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.istmo.runtime.AdMobCodecsImpl
import dev.istmo.runtime.AdMobDispatcher
import dev.istmo.runtime.AdMobFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NotificationsBackendImpl
import dev.istmo.runtime.NotificationsCodecsImpl
import dev.istmo.runtime.NotificationsDispatcher
import dev.istmo.runtime.PermissionsBackendImpl
import dev.istmo.runtime.PermissionsCodecsImpl
import dev.istmo.runtime.PermissionsDispatcher
import dev.istmo.runtime.SignInCodecsImpl
import dev.istmo.runtime.SignInDispatcher
import dev.istmo.runtime.SignInFactoryImpl

/**
 * NativeActivity subclass that boots the istmo runtime and registers the
 * plugin dispatchers the Rust cdylib consumes, all *before* the native
 * activity's onCreate runs `android_main`.
 *
 * `<meta-data android:name="android.app.lib_name" android:value="rust_mobile_demo"/>`
 * in the manifest tells NativeActivity which cdylib to load. We separately
 * call `IstmoRuntime.start()`, which:
 *
 *  1. Loads the same library via `System.loadLibrary` (idempotent).
 *  2. Runs `nativeStart` — invokes `__istmo_configure_runtime` emitted by
 *     `istmo::runtime!` in the Rust cdylib, initialising the process
 *     `Runtime`.
 *  3. Spawns the pump thread that drains outbound frames.
 *
 * Each plugin follows the codegen dispatcher pattern:
 * `<T>Dispatcher(factoryOrBackend, codecs)` — the dispatcher owns wire
 * encode / decode, the backend owns the SDK-specific logic.
 */
class RustMobileActivity : NativeActivity() {

    private lateinit var permissionsBackend: PermissionsBackendImpl
    private lateinit var signInFactory: SignInFactoryImpl

    override fun onCreate(savedInstanceState: Bundle?) {
        // Register plugin dispatchers BEFORE super.onCreate() — the Rust
        // side may fire calls as soon as its NDK glue thread starts.
        val runtime = IstmoRuntime
        val ok = runtime.start()
        check(ok) { "IstmoRuntime.start() failed — pump did not initialise" }

        permissionsBackend = PermissionsBackendImpl(this)
        signInFactory = SignInFactoryImpl(this)

        runtime.registerHandler(
            PermissionsDispatcher.PLUGIN_ID,
            PermissionsDispatcher(permissionsBackend, PermissionsCodecsImpl()),
        )
        runtime.registerHandler(
            NotificationsDispatcher.PLUGIN_ID,
            NotificationsDispatcher(NotificationsBackendImpl(this), NotificationsCodecsImpl()),
        )
        runtime.registerHandler(
            SignInDispatcher.PLUGIN_ID,
            SignInDispatcher(signInFactory, SignInCodecsImpl()),
        )
        runtime.registerHandler(
            AdMobDispatcher.PLUGIN_ID,
            AdMobDispatcher(AdMobFactoryImpl(this), AdMobCodecsImpl()),
        )

        super.onCreate(savedInstanceState)

        // Edge-to-edge — the Rust side reads safe-area insets from the
        // `istmo.safe_area` early-event channel (Flutter-style) and
        // reserves its own padding. Decor no longer manages the fit;
        // system bars stay translucent overlays above our surface.
        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            show(WindowInsetsCompat.Type.systemBars())
            isAppearanceLightStatusBars = false
        }
        installSafeAreaListener()
    }

    /**
     * Subscribe to Android's `WindowInsets` and publish every update on
     * the `istmo.safe_area` early-event channel in logical dp. Flutter
     * model — platform pushes safe-area geometry, framework (egui) reads
     * a snapshot each frame.
     */
    private fun installSafeAreaListener() {
        val density = resources.displayMetrics.density
        val root = window.decorView
        ViewCompat.setOnApplyWindowInsetsListener(root) { _, insets ->
            val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars())
            val ime = insets.getInsets(WindowInsetsCompat.Type.ime())
            val cutout = insets.getInsets(WindowInsetsCompat.Type.displayCutout())
            IstmoRuntime.publishSafeArea(
                bars.top / density, bars.right / density, bars.bottom / density, bars.left / density,
                ime.top / density, ime.right / density, ime.bottom / density, ime.left / density,
                cutout.top / density, cutout.right / density, cutout.bottom / density, cutout.left / density,
            )
            insets
        }
        // Force an initial dispatch — otherwise the callback only fires on
        // the first inset *change*, and Rust would start with a `None`
        // slot until the user rotates or opens the keyboard.
        ViewCompat.requestApplyInsets(root)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissionsRequested: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissionsRequested, grantResults)
        permissionsBackend.notifyPermissionsResult(requestCode, permissionsRequested, grantResults)
    }

    @Deprecated("legacy GoogleSignInClient uses the pre-ActivityResult API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        signInFactory.rememberLastBackend()?.notifyActivityResult(requestCode, resultCode, data)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
