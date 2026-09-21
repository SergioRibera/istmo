package dev.istmo.pendemo

import android.app.NativeActivity
import android.os.Bundle
import android.view.ViewGroup
import dev.istmo.plugins.pen.PenBackendImpl
import dev.istmo.plugins.pen.PenCaptureView
import dev.istmo.plugins.pen.PenFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PenCodecsImpl
import dev.istmo.runtime.PenDispatcher

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

        // Overlay the pen view above the NativeActivity surface so it
        // receives MotionEvents before the native window sees them. The
        // native surface stays visible underneath and can render, but
        // for this diagnostic demo it stays blank — samples surface via
        // `logcat`.
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
