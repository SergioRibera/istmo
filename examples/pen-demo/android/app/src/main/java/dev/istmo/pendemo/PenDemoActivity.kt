package dev.istmo.pendemo

import android.app.NativeActivity
import android.os.Bundle
import android.view.ViewGroup
import dev.istmo.plugins.pen.PenCaptureView
import dev.istmo.plugins.pen.PenFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PenCodecsImpl
import dev.istmo.runtime.PenDispatcher

/**
 * NativeActivity so winit / eframe (running in `android_main` on the
 * Rust side) owns the drawing surface. A transparent
 * [PenCaptureView] sits above the native surface, intercepts stylus
 * `MotionEvent`s, and forwards them across the istmo wire as
 * `PenEvent`s — the Rust `DrawApp` reads that stream and renders
 * strokes.
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

        val params = ViewGroup.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT,
            ViewGroup.LayoutParams.MATCH_PARENT,
        )
        addContentView(penView, params)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
