package dev.istmo.demo

import android.content.Intent
import android.os.Bundle
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts.RequestMultiplePermissions
import androidx.appcompat.app.AppCompatActivity
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext

class MainActivity : AppCompatActivity(), PermissionsHost, ActivityResultsHost {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private val permissionsImpl = PermissionsImpl()
    private val activityResultsImpl = ActivityResultsImpl()
    private var pendingPermissionResult: ((Map<String, Boolean>) -> Unit)? = null
    private var initialized = false

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
            IstmoRuntime.registerHandler("dev.istmo.demo.echo", EchoImpl())
            IstmoRuntime.registerHandler("istmo.permissions", permissionsImpl)
            IstmoRuntime.registerHandler("istmo.activity_results", activityResultsImpl)
            IstmoRuntime.start()
            initialized = true
        }
        permissionsImpl.attach(this)
        activityResultsImpl.attach(this)

        DemoBridge.pushLifecycle(LifecycleState.Created.ordinal)
        handleIntent(intent)
        wireEchoSection()
        wireLifecycleSection()
        wireDeepLinksSection()
        wirePermissionsSection()
        wireActivityResultsSection()
    }

    override fun onStart() {
        super.onStart()
        DemoBridge.pushLifecycle(LifecycleState.Started.ordinal)
    }

    override fun onResume() {
        super.onResume()
        DemoBridge.pushLifecycle(LifecycleState.Resumed.ordinal)
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
        permissionsImpl.detach()
        activityResultsImpl.detach()
        scope.cancel()
        super.onDestroy()
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
        val refresh = findViewById<Button>(R.id.deeplinkRefresh)
        refresh.setOnClickListener {
            drainDeepLinks()
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
                val ordinal = withContext(Dispatchers.IO) { DemoBridge.callCheckPermission(perm) }
                output.text = "check($perm) -> ${statusName(ordinal)}"
            }
        }
        requestBtn.setOnClickListener {
            val perm = input.text.toString()
            scope.launch {
                val ordinal = withContext(Dispatchers.IO) { DemoBridge.callRequestPermission(perm) }
                output.text = "request($perm) -> ${statusName(ordinal)}"
            }
        }
    }

    private fun statusName(ordinal: Int): String =
        PermissionStatusValue.values().getOrNull(ordinal)?.name
            ?: "error(ordinal=$ordinal)"

    private fun wireActivityResultsSection() {
        val input = findViewById<EditText>(R.id.intentUri)
        val launch = findViewById<Button>(R.id.launchIntent)
        val output = findViewById<TextView>(R.id.intentOutput)
        launch.setOnClickListener {
            val uri = input.text.toString().ifBlank { null }
            scope.launch {
                val result = withContext(Dispatchers.IO) {
                    DemoBridge.callLaunchIntent(Intent.ACTION_VIEW, uri)
                }
                output.text = result
            }
        }
    }

    private fun drainDeepLinks() {
        val view = findViewById<TextView>(R.id.deeplinkList) ?: return
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
