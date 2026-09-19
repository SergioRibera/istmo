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

class RustMobileActivity : NativeActivity() {

    private lateinit var permissionsBackend: PermissionsBackendImpl
    private lateinit var signInFactory: SignInFactoryImpl

    override fun onCreate(savedInstanceState: Bundle?) {

        val runtime = IstmoRuntime
        val ok = runtime.start("rust_mobile_demo")
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

        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            show(WindowInsetsCompat.Type.systemBars())
            isAppearanceLightStatusBars = false
        }
        installSafeAreaListener()
    }

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

