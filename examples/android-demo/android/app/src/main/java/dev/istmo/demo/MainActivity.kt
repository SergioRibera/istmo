package dev.istmo.demo

import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : AppCompatActivity() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var initialized = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        if (!initialized) {
            IstmoRuntime.registerHandler("dev.istmo.demo.echo", EchoImpl())
            // `nativeStart` returns false on subsequent invocations (e.g. after
            // a config change). Ignore — the runtime is already up.
            IstmoRuntime.start()
            initialized = true
        }

        val input = findViewById<EditText>(R.id.inputText)
        val button = findViewById<Button>(R.id.callButton)
        val output = findViewById<TextView>(R.id.output)

        button.setOnClickListener {
            val text = input.text.toString()
            scope.launch {
                val response: String = withContext(Dispatchers.IO) {
                    DemoBridge.callEcho(text)
                }
                output.text = response
            }
        }
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
        // NB: the process-global istmo runtime intentionally outlives the
        // Activity. `nativeShutdown` is left to the process's tear-down.
    }
}
