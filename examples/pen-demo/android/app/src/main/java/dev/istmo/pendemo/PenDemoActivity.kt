package dev.istmo.pendemo

import android.app.NativeActivity
import android.os.Bundle
import android.view.ViewGroup
import dev.istmo.plugins.pen.PenFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PenCodecsImpl
import dev.istmo.runtime.PenDispatcher

class PenDemoActivity : NativeActivity() {

    private lateinit var penView: PenDrawingView

    override fun onCreate(savedInstanceState: Bundle?) {
        val runtime = IstmoRuntime
        check(runtime.start("pen_demo")) { "IstmoRuntime.start() failed" }

        penView = PenDrawingView(this)

        runtime.registerHandler(
            PenDispatcher.PLUGIN_ID,
            PenDispatcher(PenFactoryImpl(penView), PenCodecsImpl()),
        )

        super.onCreate(savedInstanceState)

        // Overlay the drawing view above the NativeActivity surface so
        // it receives MotionEvents before the native window sees them.
        // The drawing view is white-backed and covers the whole screen —
        // strokes appear as the user draws with a stylus, and the same
        // events also travel through the istmo wire so the Rust side
        // logs them to `adb logcat`.
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
