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
import dev.istmo.runtime.GoogleSignInHandler
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.NotificationsHandler
import dev.istmo.runtime.PermissionsHandler

/**
 * NativeActivity subclass that boots the istmo runtime and registers the
 * three plugin dispatchers the Rust cdylib consumes, all *before* the
 * native activity's onCreate runs `android_main`.
 *
 * `<meta-data android:name="android.app.lib_name" android:value="rust_mobile_demo"/>`
 * in the manifest tells NativeActivity which cdylib to load. We separately
 * call `IstmoRuntime.start()`, which:
 *
 *  1. Loads the same library via `System.loadLibrary` (idempotent).
 *  2. Runs `nativeStart` — which invokes `__istmo_configure_runtime`
 *     emitted by `istmo::runtime!` in the Rust cdylib, initialising the
 *     process-global `Runtime`.
 *  3. Spawns the pump thread that drains outbound frames.
 *
 * The Rust `android_main` (in `src/android.rs`) then fires on the NDK
 * glue thread and starts eframe. Every plugin call `SignInClient::…` /
 * `PermissionsClient::…` / `NotificationsClient::…` issues an outbound
 * `Frame::Call`; the pump hands it to `IstmoRuntime.onCall` which routes
 * to the dispatcher registered under the matching plugin id.
 */
class RustMobileActivity : NativeActivity() {

    private lateinit var permissions: PermissionsHandler
    private lateinit var signIn: GoogleSignInHandler

    override fun onCreate(savedInstanceState: Bundle?) {
        // Register plugin dispatchers BEFORE super.onCreate() — the Rust
        // side may fire calls as soon as its NDK glue thread starts.
        val runtime = IstmoRuntime
        val ok = runtime.start()
        check(ok) { "IstmoRuntime.start() failed — pump did not initialise" }

        permissions = PermissionsHandler(this)
        signIn = GoogleSignInHandler(this)
        runtime.registerHandler(GoogleSignInHandler.PLUGIN_ID, signIn)
        runtime.registerHandler(NotificationsHandler.PLUGIN_ID, NotificationsHandler(this))
        runtime.registerHandler(PermissionsHandler.PLUGIN_ID, permissions)
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
     * the `istmo.safe_area` early-event channel in logical dp. This is
     * the Flutter model: platform pushes safe-area geometry, framework
     * (here egui) reads a snapshot each frame.
     *
     * We forward three inset groups separately so the Rust side can pick
     * the ones it cares about — a full-bleed video player only respects
     * cutouts, a conventional layout takes the union of system bars +
     * cutout, an IME-aware field uses `view_padding` to lift above the
     * keyboard.
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
        permissions.notifyPermissionsResult(requestCode, permissionsRequested, grantResults)
    }

    @Deprecated("legacy GoogleSignInClient uses the pre-ActivityResult API")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        signIn.notifyActivityResult(requestCode, resultCode, data)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
