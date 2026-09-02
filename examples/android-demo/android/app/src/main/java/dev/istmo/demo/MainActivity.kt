package dev.istmo.demo

import android.content.Intent
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions
import androidx.appcompat.app.AppCompatActivity
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
 * [IstmoRuntime]. Echo is called via [EchoClient] (hand-written stand-in for
 * codegen). Lifecycle transitions and deep links get pushed as early
 * events. Permissions / ActivityResults registrations stay so a future
 * Rust host method can invoke them; the demo UI does not trigger them
 * yet because that path requires a Rust host method exposing the flow.
 */
class MainActivity : AppCompatActivity(), PermissionsHost, ActivityResultsHost {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val permissionsImpl = PermissionsImpl()
    private val activityResultsImpl = ActivityResultsImpl()
    private var pendingPermissionResult: ((Map<String, Boolean>) -> Unit)? = null
    private var initialized = false
    private val deeplinkHistory = mutableListOf<String>()

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
            IstmoRuntime.start()
            initialized = true
        }
        permissionsImpl.attach(this)
        activityResultsImpl.attach(this)

        LifecycleState.Created.publish()
        handleIntent(intent)
        wireEchoSection()
        wireDeepLinksSection()
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
                output.text = try {
                    EchoClient.echo(text)
                } catch (e: EchoException) {
                    "error: ${e.reason}"
                } catch (t: Throwable) {
                    "transport error: ${t.message}"
                }
            }
        }
    }

    private fun wireDeepLinksSection() {
        findViewById<Button>(R.id.deeplinkRefresh).setOnClickListener { renderDeepLinks() }
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
        view.text = deeplinkHistory.joinToString("\n")
    }

    /** Bincode-encode a `DeepLink { uri, source: Option<String>, received_at_ms: Option<u64> }`. */
    private fun encodeDeepLink(uri: String, source: String?): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, uri)
        Bincode.writeOption(out, source) { sink, s -> Bincode.writeString(sink, s) }
        Bincode.writeOption(out, null as Long?) { sink, v -> Bincode.writeVarintU64(sink, v) }
        return out.toByteArray()
    }
}
