package dev.istmo.demo

import android.content.Intent
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions
import androidx.appcompat.app.AppCompatActivity
import dev.istmo.demo.gen.EchoClient
import dev.istmo.demo.gen.EchoCodecsImpl
import dev.istmo.demo.gen.EchoException
import dev.istmo.demo.gen.NotifierDispatcher
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import java.io.ByteArrayOutputStream

/**
 * Demo Activity — every Rust ↔ Kotlin crossing goes through
 * [IstmoRuntime]. Each UI button invokes an [EchoClient] method whose Rust
 * implementation internally consumes the corresponding client plugin
 * (`PermissionsClient`, `ActivityResultsClient`, `AppLifecycle`,
 * `DeepLinks`). Response then round-trips back through the frame protocol.
 * Lifecycle transitions and deep links get pushed as early events on the
 * way in.
 */
class MainActivity : AppCompatActivity(), PermissionsHost, ActivityResultsHost {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val permissionsImpl = PermissionsImpl()
    private val activityResultsImpl = ActivityResultsImpl()
    private val echoClient = EchoClient(EchoCodecsImpl())
    private val notifierBackend = NotifierBackendImpl { msg -> appendNotifierLine(msg) }
    private val notifierDispatcher = NotifierDispatcher(notifierBackend, NotifierCodecsAdapter())
    private var pendingPermissionResult: ((Map<String, Boolean>) -> Unit)? = null
    private var initialized = false
    private val deeplinkHistory = mutableListOf<String>()
    private val notifierLog = mutableListOf<String>()

    private val permissionsLauncher =
        registerForActivityResult(RequestMultiplePermissions()) { results ->
            pendingPermissionResult?.invoke(results)
            pendingPermissionResult = null
        }

    override val activity get() = this

    override fun request(permissions: List<String>, onResult: (Map<String, Boolean>) -> Unit) {
        pendingPermissionResult = onResult
        permissionsLauncher.launch(permissions.toTypedArray())
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        if (!initialized) {
            IstmoRuntime.registerHandler("istmo.permissions", permissionsImpl)
            IstmoRuntime.registerHandler("istmo.activity_results", activityResultsImpl)
            IstmoRuntime.registerHandler(NotifierDispatcher.PLUGIN_ID, notifierDispatcher)
            IstmoRuntime.start()
            initialized = true
        }
        permissionsImpl.attach(this)
        activityResultsImpl.attach(this)

        LifecycleState.Created.publish()
        handleIntent(intent)
        wireEchoSection()
        wirePermissionsSection()
        wireActivitySection()
        wireLifecycleSection()
        wireDeepLinksSection()
        wireNotifierSection()
    }

    override fun onStart() { super.onStart(); LifecycleState.Started.publish() }
    override fun onResume() {
        super.onResume(); LifecycleState.Resumed.publish()
        renderDeepLinks()
    }
    override fun onPause() { LifecycleState.Paused.publish(); super.onPause() }
    override fun onStop() { LifecycleState.Stopped.publish(); super.onStop() }
    override fun onLowMemory() { super.onLowMemory(); LifecycleState.LowMemory.publish() }
    override fun onConfigurationChanged(newConfig: android.content.res.Configuration) {
        super.onConfigurationChanged(newConfig)
        LifecycleState.ConfigurationChanged.publish()
    }
    override fun onDestroy() {
        LifecycleState.Destroyed.publish()
        permissionsImpl.detach()
        activityResultsImpl.detach()
        scope.cancel()
        super.onDestroy()
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        setIntent(intent)
        handleIntent(intent)
    }

    // ---- Section wiring ----

    private fun wireEchoSection() {
        val input = findViewById<EditText>(R.id.inputText)
        val button = findViewById<Button>(R.id.callButton)
        val output = findViewById<TextView>(R.id.output)

        button.setOnClickListener {
            val text = input.text.toString()
            scope.launch {
                output.text = runCatchingEcho { echoClient.echo(text) }
            }
        }
    }

    private fun wirePermissionsSection() {
        val input = findViewById<EditText>(R.id.permissionInput)
        val checkBtn = findViewById<Button>(R.id.permissionCheck)
        val requestBtn = findViewById<Button>(R.id.permissionRequest)
        val output = findViewById<TextView>(R.id.permissionOutput)

        checkBtn.setOnClickListener {
            val perm = input.text.toString()
            scope.launch {
                output.text = runCatchingEcho { echoClient.checkPermission(perm) }
            }
        }
        requestBtn.setOnClickListener {
            val perm = input.text.toString()
            scope.launch {
                output.text = runCatchingEcho { echoClient.requestPermission(perm) }
            }
        }
    }

    private fun wireActivitySection() {
        val input = findViewById<EditText>(R.id.intentUri)
        val launch = findViewById<Button>(R.id.launchIntent)
        val output = findViewById<TextView>(R.id.intentOutput)
        launch.setOnClickListener {
            val url = input.text.toString().ifBlank { "https://example.com" }
            scope.launch {
                output.text = runCatchingEcho { echoClient.openUrl(url) }
            }
        }
    }

    private fun wireLifecycleSection() {
        val label = findViewById<TextView>(R.id.lifecycleState)
        val refresh = findViewById<Button>(R.id.lifecycleRefresh)
        refresh.setOnClickListener {
            scope.launch {
                label.text = runCatchingEcho { echoClient.lifecycleSnapshot() }
            }
        }
    }

    private fun wireDeepLinksSection() {
        val output = findViewById<TextView>(R.id.deeplinkList)
        val refresh = findViewById<Button>(R.id.deeplinkRefresh)
        refresh.setOnClickListener {
            scope.launch {
                try {
                    val links = echoClient.drainDeeplinks()
                    output.text = if (links.isEmpty()) "(none drained)" else links.joinToString("\n")
                } catch (e: EchoException) {
                    output.text = "error: ${e.decoded?.reason ?: "unknown"}"
                } catch (t: Throwable) {
                    output.text = "transport error: ${t.message}"
                }
            }
        }
    }

    private fun handleIntent(intent: Intent?) {
        val data = intent?.data?.toString() ?: return
        if (intent.action == Intent.ACTION_VIEW) {
            IstmoRuntime.submitEarlyQueue(
                channel = "istmo.deeplinks",
                capacity = 16,
                payload = encodeDeepLink(uri = data, source = "intent"),
            )
            deeplinkHistory += data
            renderDeepLinks()
        }
    }

    private fun renderDeepLinks() {
        val view = findViewById<TextView>(R.id.deeplinkList) ?: return
        if (deeplinkHistory.isEmpty()) return
        view.text = "arrived (before drain):\n" + deeplinkHistory.joinToString("\n")
    }

    /** Bincode-encode a `DeepLink { uri, source: Option<String>, received_at_ms: Option<u64> }`. */
    private fun encodeDeepLink(uri: String, source: String?): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, uri)
        Bincode.writeOption(out, source) { sink, s -> Bincode.writeString(sink, s) }
        Bincode.writeOption(out, null as Long?) { sink, v -> Bincode.writeVarintU64(sink, v) }
        return out.toByteArray()
    }

    private inline fun runCatchingEcho(block: () -> String): String = try {
        block()
    } catch (e: EchoException) {
        "error: ${e.decoded?.reason ?: "unknown"}"
    } catch (t: Throwable) {
        "transport error: ${t.message}"
    }

    // ---- Notifier: Rust → Mobile initiator ---------------------------------

    private fun wireNotifierSection() {
        val countInput = findViewById<EditText>(R.id.notifyCount)
        val button = findViewById<Button>(R.id.notifyButton)
        val output = findViewById<TextView>(R.id.notifyOutput)
        val log = findViewById<TextView>(R.id.notifyLog)

        button.setOnClickListener {
            val count = countInput.text.toString().toUIntOrNull() ?: 3u
            notifierLog.clear()
            log.text = ""
            scope.launch {
                output.text = try {
                    val fired = echoClient.spamNotify(count)
                    "fired $fired notifications (Rust → Mobile)"
                } catch (e: EchoException) {
                    "error: ${e.decoded?.reason ?: "unknown"}"
                } catch (t: Throwable) {
                    "transport error: ${t.message}"
                }
            }
        }
    }

    private fun appendNotifierLine(message: String) {
        notifierLog += message
        runOnUiThread {
            findViewById<TextView>(R.id.notifyLog)?.text =
                notifierLog.joinToString("\n")
        }
    }
}
