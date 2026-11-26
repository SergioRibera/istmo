package dev.istmo.screencaptureprobe

import android.content.Intent
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioTrack
import android.os.Bundle
import android.view.SurfaceHolder
import android.view.SurfaceView
import android.view.ViewGroup.LayoutParams.MATCH_PARENT
import android.view.ViewGroup.LayoutParams.WRAP_CONTENT
import android.widget.Button
import android.widget.HorizontalScrollView
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.activity.ComponentActivity
import dev.istmo.runtime.IstmoHost
import dev.istmo.runtime.IstmoRuntime
import java.util.concurrent.Executors
import kotlin.math.PI
import kotlin.math.sin

/** The probe's Rust entry (`src/lib.rs`). */
object Probe {
    external fun run(command: String, surface: Long): String
}

/**
 * Runs the probe commands from buttons, or from an intent extra:
 * `adb shell am start -n dev.istmo.screencaptureprobe/.ProbeActivity --es cmd "stream 5"`.
 * Reports show here and in logcat (`capture-probe`).
 */
class ProbeActivity : ComponentActivity(), SurfaceHolder.Callback {
    private lateinit var log: TextView
    private lateinit var scroll: ScrollView
    private val worker = Executors.newSingleThreadExecutor()

    @Volatile
    private var surfaceHandle = 0L

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
        val preview = SurfaceView(this)
        preview.holder.addCallback(this)
        log = TextView(this).apply {
            textSize = 11f
            setTextIsSelectable(true)
        }
        scroll = ScrollView(this).apply { addView(log) }
        setContentView(LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            addView(HorizontalScrollView(this@ProbeActivity).apply { addView(buttons) }, MATCH_PARENT, WRAP_CONTENT)
            addView(preview, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
            addView(scroll, LinearLayout.LayoutParams(MATCH_PARENT, 0, 1f))
        })
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
            val tone = if (command.startsWith("audio")) Tone().also { it.start() } else null
            val report = try {
                Probe.run(command, surfaceHandle)
            } finally {
                tone?.stop()
            }
            runOnUiThread { append(report) }
        }
    }

    private fun append(text: String) {
        log.append(text)
        log.append("\n")
        scroll.post { scroll.fullScroll(ScrollView.FOCUS_DOWN) }
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        surfaceHandle = IstmoRuntime.registerObject(holder.surface)
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {}

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        IstmoRuntime.releaseObject(surfaceHandle)
        surfaceHandle = 0L
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoHost.onDestroy(this)
    }

    /** A 440 Hz tone played as media, so playback capture has something to hear. */
    private class Tone {
        private val rate = 48_000
        private val track: AudioTrack

        init {
            val samples = ShortArray(rate * 2)
            for (i in 0 until rate) {
                val value = (sin(2 * PI * 440 * i / rate) * Short.MAX_VALUE * 0.3).toInt().toShort()
                samples[2 * i] = value
                samples[2 * i + 1] = value
            }
            track = AudioTrack.Builder()
                .setAudioAttributes(
                    AudioAttributes.Builder()
                        .setUsage(AudioAttributes.USAGE_MEDIA)
                        .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                        .build(),
                )
                .setAudioFormat(
                    AudioFormat.Builder()
                        .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                        .setSampleRate(rate)
                        .setChannelMask(AudioFormat.CHANNEL_OUT_STEREO)
                        .build(),
                )
                .setTransferMode(AudioTrack.MODE_STATIC)
                .setBufferSizeInBytes(samples.size * 2)
                .build()
            track.write(samples, 0, samples.size)
            track.setLoopPoints(0, rate, -1)
        }

        fun start() = track.play()

        fun stop() {
            track.stop()
            track.release()
        }
    }

    private companion object {
        const val LIBRARY = "screen_capture_probe"
        const val EXTRA_COMMAND = "cmd"
        val COMMANDS = listOf(
            "caps", "stream", "stream-cpu", "stream-max15", "stream-exact10", "hold", "shot", "shot-gpu",
            "pixels", "preview", "fanout", "audio", "audio-only", "grant", "refuse",
        )
    }
}
