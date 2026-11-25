package dev.istmo.bench

import android.app.Activity
import android.graphics.Typeface
import android.os.Bundle
import android.util.Log
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import dev.istmo.bench.gen.KotlinBenchCodecsImpl
import dev.istmo.bench.gen.KotlinBenchDispatcher
import dev.istmo.runtime.IstmoRuntime
import kotlin.concurrent.thread

/**
 * Bench UI. Drive it from a shell with
 * `adb shell am start -n dev.istmo.bench/.BenchActivity --ez auto true [--es mode probe]`
 * and scrape logcat (tag [LOG_TAG], bracketed by [LOG_BEGIN] / [LOG_END]).
 *
 * Modes, one per process (the runtime starts once per process):
 * - `full` (default): starts the runtime the way apps do
 *   (`IstmoRuntime.start`, ABI handshake included) and runs the matrix;
 * - `probe`: starts it with [ProbeRuntime] as the pump's callback class
 *   and runs the per-hop breakdown;
 * - `legacy`: the original batch-mean matrix only.
 */
class BenchActivity : Activity() {
    private lateinit var output: TextView
    private lateinit var runButton: Button
    private val shown = StringBuilder()

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // MIUI refuses `svc power stayon` from adb: keep the screen on
        // from here so the app stays foreground for the whole run.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(24, 24, 24, 24)
        }
        runButton = Button(this).apply {
            text = "Run benchmark matrix"
            setOnClickListener { startRun(intent?.getStringExtra("mode") ?: "full") }
        }
        output = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 9f
        }
        root.addView(runButton)
        root.addView(ScrollView(this).apply { addView(output) })
        setContentView(root)

        if (intent?.getBooleanExtra("auto", false) == true) {
            startRun(intent?.getStringExtra("mode") ?: "full")
        }
    }

    private fun emit(line: String) {
        Log.i(LOG_TAG, line)
        runOnUiThread {
            shown.append(line).append('\n')
            output.text = shown
        }
    }

    private fun startRun(mode: String) {
        runButton.isEnabled = false
        thread(name = "istmo-bench") {
            emit(LOG_BEGIN)
            emit("mode: $mode")
            try {
                start(mode)
                val runner = BenchRunner(::emit)
                runner.environment()
                runner.clockCheck()
                when (mode) {
                    "probe" -> runner.breakdown()
                    "legacy" -> runner.legacy()
                    else -> {
                        runner.matrix()
                        runner.streams()
                        runner.wakeups()
                        runner.frames()
                        runner.sensitivity()
                    }
                }
            } catch (e: Throwable) {
                emit("FATAL: $e")
                Log.e(LOG_TAG, "bench failed", e)
            }
            emit(LOG_END)
            runOnUiThread { runButton.isEnabled = true }
        }
    }

    private fun start(mode: String) {
        if (started) return
        started = true
        val dispatcher = KotlinBenchDispatcher(BenchBackend(), KotlinBenchCodecsImpl())
        if (mode == "probe") {
            System.loadLibrary(LIBRARY)
            emit("abi: ${checkAbi()}")
            IstmoRuntime.nativeStart(ProbeRuntime::class.java)
            IstmoRuntime.registerHandler(KotlinBenchDispatcher.PLUGIN_ID, ProbeHandler(dispatcher))
        } else {
            // The app path: loads the library, checks the native ABI, starts the pump.
            IstmoRuntime.start(applicationContext, LIBRARY)
            emit("abi: IstmoRuntime.start passed the handshake (${checkAbi()})")
            IstmoRuntime.registerHandler(KotlinBenchDispatcher.PLUGIN_ID, dispatcher)
        }
    }

    /** Run the runtime's own ABI check (`IstmoFrames` is internal to the runtime module). */
    private fun checkAbi(): String {
        val frames = Class.forName("dev.istmo.runtime.IstmoFrames")
        val instance = frames.getField("INSTANCE").get(null)
        frames.getMethod("checkAbi").invoke(instance)
        val native = frames.getMethod("nativeAbiVersion").invoke(instance)
        return "native=$native runtime=${frames.getField("ABI_VERSION").get(null)} ok"
    }

    companion object {
        const val LOG_TAG = "istmo-bench"
        const val LOG_BEGIN = "BENCH_BEGIN"
        const val LOG_END = "BENCH_END"
        private const val LIBRARY = "bench_android"

        @Volatile
        private var started = false
    }
}
