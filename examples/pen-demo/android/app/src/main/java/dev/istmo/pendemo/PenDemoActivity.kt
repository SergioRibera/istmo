package dev.istmo.pendemo

import android.app.NativeActivity
import android.os.Bundle
import android.view.ViewGroup
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import androidx.core.view.WindowInsetsControllerCompat
import dev.istmo.plugins.pen.PenCaptureView
import dev.istmo.plugins.pen.PenFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PenCodecsImpl
import dev.istmo.runtime.PenDispatcher

/**
 * NativeActivity so winit / eframe (in `android_main` on the Rust
 * side) owns the drawing surface. A transparent [PenCaptureView] sits
 * above the native surface, intercepts stylus `MotionEvent`s, and
 * forwards them across the istmo wire as `PenEvent`s. The activity
 * also installs a `WindowInsets` listener that publishes the
 * device's safe-area insets on the `istmo.safe_area` early-event
 * channel so the Rust `DrawApp` can inset its canvas around the
 * status bar / display cutout.
 */
class PenDemoActivity : NativeActivity() {

    private lateinit var penView: PenCaptureView

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        check(runtime.start("pen_demo")) { "IstmoRuntime.start() failed" }

        penView = PenCaptureView(this)
        runtime.registerHandler(
            PenDispatcher.PLUGIN_ID,
            PenDispatcher(PenFactoryImpl(penView), PenCodecsImpl()),
        )

        super.onCreate(savedInstanceState)

        WindowCompat.setDecorFitsSystemWindows(window, false)
        WindowInsetsControllerCompat(window, window.decorView).apply {
            show(WindowInsetsCompat.Type.systemBars())
            isAppearanceLightStatusBars = false
        }
        installSafeAreaListener()

        val params = ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT,
        )
        addContentView(penView, params)
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

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
