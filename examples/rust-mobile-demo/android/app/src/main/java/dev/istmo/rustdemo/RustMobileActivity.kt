package dev.istmo.rustdemo

import android.app.NativeActivity
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.view.View
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.istmo.runtime.AdMobHandler
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
        runtime.registerHandler(AdMobHandler.PLUGIN_ID, AdMobHandler(this))

        super.onCreate(savedInstanceState)

        // Show system bars and reserve their space so the egui surface
        // does not draw underneath the status bar. NativeActivity draws
        // edge-to-edge by default on newer Android; opting out here
        // matches "normal app" chrome.
        WindowCompat.setDecorFitsSystemWindows(window, true)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            show(WindowInsetsCompat.Type.systemBars())
            isAppearanceLightStatusBars = false
        }
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.R) {
            @Suppress("DEPRECATION")
            window.decorView.systemUiVisibility =
                View.SYSTEM_UI_FLAG_LAYOUT_STABLE
        }
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
