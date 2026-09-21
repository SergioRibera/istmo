package dev.istmo.pendemo

import android.os.Bundle
import androidx.appcompat.app.AppCompatActivity
import dev.istmo.plugins.pen.PenFactoryImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PenCodecsImpl
import dev.istmo.runtime.PenDispatcher

/**
 * Regular (non-native) activity — `PenDrawingView` is the content view
 * and receives stylus [MotionEvent]s directly. The Rust `.so` is loaded
 * by `IstmoRuntime.start("pen_demo")`, whose JNI pump handles inbound
 * calls without any dependency on `android_main` / NativeActivity's
 * event loop.
 */
class PenDemoActivity : AppCompatActivity() {

    private lateinit var penView: PenDrawingView

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)

        val runtime = IstmoRuntime
        check(runtime.start("pen_demo")) { "IstmoRuntime.start() failed" }

        penView = PenDrawingView(this)
        runtime.registerHandler(
            PenDispatcher.PLUGIN_ID,
            PenDispatcher(PenFactoryImpl(penView), PenCodecsImpl()),
        )

        setContentView(penView)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
