package dev.istmo.notificationsprobe

import android.content.Intent
import android.os.Bundle
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.ComponentActivity
import dev.istmo.runtime.IstmoHost
import java.util.concurrent.Executors

/** The probe's Rust entry (`src/lib.rs`). */
object Probe {
    external fun run(command: String): String
}

/**
 * Runs the probe commands from buttons, or from an intent extra:
 * `adb shell am start -n dev.istmo.notificationsprobe/.ProbeActivity --es cmd show`.
 * Reports show here and in logcat (`notif-probe`).
 */
class ProbeActivity : ComponentActivity() {
    private lateinit var log: TextView
    private lateinit var scroll: ScrollView
    private val worker = Executors.newSingleThreadExecutor()

    override fun onCreate(savedInstanceState: Bundle?) {
        IstmoHost.onCreate(this, LIBRARY)
        super.onCreate(savedInstanceState)
        val buttons = LinearLayout(this).apply { orientation = LinearLayout.HORIZONTAL }
        for (command in COMMANDS) {
            buttons.addView(Button(this).apply {
                text = command
                setOnClickListener { run(command) }
            })
        }
        log = TextView(this).apply {
            textSize = 11f
            setTextIsSelectable(true)
        }
        scroll = ScrollView(this).apply { addView(log) }
        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(HorizontalScrollView(this@ProbeActivity).apply { addView(buttons) }, MATCH_PARENT, WRAP_CONTENT)
            addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        })
        run("listen")
        handle(intent)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(intent: Intent?) {
        intent?.getStringExtra(EXTRA_COMMAND)?.let(::run)
    }

    private fun run(command: String) {
        append("> $command")
        worker.execute {
            val report = Probe.run(command)
            runOnUiThread { append(report) }
        }
    }

    private fun append(text: String) {
        log.append(text)
        log.append("\n")
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoHost.onDestroy(this)
    }

    private companion object {
        const val LIBRARY = "notifications_probe"
        const val EXTRA_COMMAND = "cmd"
        val COMMANDS = listOf(
            "caps", "perm", "ask", "show", "styles", "chat", "actions", "call", "progress", "segments",
            "group", "schedule", "pending", "delivered", "events", "cancel-all",
        )
    }
}
