package dev.istmo.bench

import android.app.Activity
import android.graphics.Typeface
import android.os.Build
import android.os.Bundle
import android.view.View
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import kotlin.concurrent.thread

class BenchActivity : Activity() {
    private lateinit var output: TextView
    private lateinit var runButton: Button

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(24, 24, 24, 24)
        }
        runButton = Button(this).apply {
            text = "Run benchmark matrix"
            setOnClickListener { startRun() }
        }
        output = TextView(this).apply {
            typeface = Typeface.MONOSPACE
            textSize = 11f
            text = header()
        }
        root.addView(runButton)
        root.addView(ScrollView(this).apply { addView(output) })
        setContentView(root)
    }

    private fun startRun() {
        runButton.isEnabled = false
        output.text = header() + "\nrunning…\n"
        thread(name = "istmo-bench") {
            val results = BenchRunner.runFullMatrix()
            runOnUiThread {
                output.text = header() + "\n" + render(results)
                runButton.isEnabled = true
            }
        }
    }

    private fun header(): String {
        val manufacturer = Build.MANUFACTURER
        val model = Build.MODEL
        val abi = Build.SUPPORTED_ABIS.firstOrNull() ?: "?"
        val release = Build.VERSION.RELEASE
        val sdk = Build.VERSION.SDK_INT
        return "device: $manufacturer $model\nabi: $abi · android: $release (sdk $sdk)"
    }

    private fun render(results: List<BenchResult>): String {
        val sb = StringBuilder()
        sb.append("mode      payload    p50       p95       p99       min       max\n")
        sb.append("---------------------------------------------------------------\n")
        for (r in results) {
            sb.append(
                "%-8s  %7dB  %6d ns %6d ns %6d ns %6d ns %6d ns\n".format(
                    r.mode.name, r.payloadBytes, r.p50Ns, r.p95Ns, r.p99Ns, r.minNs, r.maxNs,
                ),
            )
        }
        return sb.toString()
    }
}
