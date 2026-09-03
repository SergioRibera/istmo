package dev.istmo.rustdemo

import android.app.NativeActivity
import android.os.Bundle
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

    override fun onCreate(savedInstanceState: Bundle?) {
        // Register plugin dispatchers BEFORE super.onCreate() — the Rust
        // side may fire calls as soon as its NDK glue thread starts.
        val runtime = IstmoRuntime
        val ok = runtime.start()
        check(ok) { "IstmoRuntime.start() failed — pump did not initialise" }

        permissions = PermissionsHandler(this)
        runtime.registerHandler(GoogleSignInHandler.PLUGIN_ID, GoogleSignInHandler(this))
        runtime.registerHandler(NotificationsHandler.PLUGIN_ID, NotificationsHandler(this))
        runtime.registerHandler(PermissionsHandler.PLUGIN_ID, permissions)

        super.onCreate(savedInstanceState)
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissionsRequested: Array<out String>,
        grantResults: IntArray,
    ) {
        super.onRequestPermissionsResult(requestCode, permissionsRequested, grantResults)
        permissions.notifyPermissionsResult(requestCode, permissionsRequested, grantResults)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
