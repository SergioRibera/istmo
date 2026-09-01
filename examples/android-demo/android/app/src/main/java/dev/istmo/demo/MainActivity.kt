package dev.istmo.demo

import android.content.Intent
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.appcompat.app.AppCompatActivity
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
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
            IstmoRuntime.start()
            initialized = true
        }

        DemoBridge.pushLifecycle(LifecycleState.Created.ordinal)
        handleIntent(intent)
        wireEchoSection()
        wireLifecycleSection()
        wireDeepLinksSection()
    }

    override fun onStart() {
        super.onStart()
        DemoBridge.pushLifecycle(LifecycleState.Started.ordinal)
    }

    override fun onResume() {
        super.onResume()
        DemoBridge.pushLifecycle(LifecycleState.Resumed.ordinal)
        // Kick a passive poll so any deep-link that arrived while paused shows
        // up in the UI without waiting for a button press.
        drainDeepLinks()
    }

    override fun onPause() {
        DemoBridge.pushLifecycle(LifecycleState.Paused.ordinal)
        super.onPause()
    }

    override fun onStop() {
        DemoBridge.pushLifecycle(LifecycleState.Stopped.ordinal)
        super.onStop()
    }

    override fun onDestroy() {
        DemoBridge.pushLifecycle(LifecycleState.Destroyed.ordinal)
        scope.cancel()
        super.onDestroy()
        // NB: the process-global istmo runtime intentionally outlives the
        // Activity. `nativeShutdown` is left to the process's tear-down.
    }

    override fun onLowMemory() {
        super.onLowMemory()
        DemoBridge.pushLifecycle(LifecycleState.LowMemory.ordinal)
    }

    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        DemoBridge.pushLifecycle(LifecycleState.ConfigurationChanged.ordinal)
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntent(intent)
        drainDeepLinks()
    }

    // ---- Section wiring ----

    private fun wireEchoSection() {
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

    private fun wireLifecycleSection() {
        val label = findViewById<TextView>(R.id.lifecycleState)
        val refresh = findViewById<Button>(R.id.lifecycleRefresh)
        refresh.setOnClickListener {
            val ordinal = DemoBridge.pollLifecycle()
            val state = LifecycleState.fromOrdinal(ordinal)?.name ?: "(none observed)"
            label.text = "current: $state"
        }
    }

    private fun wireDeepLinksSection() {
        val output = findViewById<TextView>(R.id.deeplinkList)
        val refresh = findViewById<Button>(R.id.deeplinkRefresh)
        refresh.setOnClickListener {
            drainDeepLinks(output)
        }
    }

    private fun drainDeepLinks(target: TextView? = findViewById(R.id.deeplinkList)) {
        val view = target ?: return
        val links = generateSequence { DemoBridge.pollDeepLink() }.toList()
        if (links.isEmpty()) return
        val existing = view.text.toString()
        val appended = (existing.split('\n').filter { it.isNotBlank() } + links).joinToString("\n")
        view.text = appended
    }

    private fun handleIntent(intent: Intent?) {
        val data = intent?.data?.toString() ?: return
        if (intent.action == Intent.ACTION_VIEW) {
            DemoBridge.pushDeepLink(data, "intent")
        }
    }
}
