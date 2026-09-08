package dev.istmo.multi

import android.Manifest
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import android.widget.Button
import android.widget.TextView
import androidx.activity.result.contract.ActivityResultContracts.RequestPermission
import androidx.appcompat.app.AppCompatActivity
import androidx.core.content.ContextCompat
import dev.istmo.multi.gen.AppControlClient
import dev.istmo.multi.gen.AppControlCodecsImpl
import dev.istmo.multi.gen.AppControlException
import dev.istmo.multi.gen.ServiceControlCodecsImpl
import dev.istmo.multi.gen.ServiceControlDispatcher
import dev.istmo.runtime.IstmoRuntime
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.launch

/**
 * Minimal UI for the multi-cdylib demo.
 *
 * Three interactions:
 *  1. `ping` — round-trips through the Rust-hosted `AppControl` trait.
 *  2. `Start Sync` — starts the [SyncService] foreground service; the
 *     Rust `SyncImpl` uses its [`ServiceContext`] to `startForeground`
 *     and update the notification every 2s.
 *  3. `Stop Sync` — asks the service to stop, which flips
 *     `ServiceContext::stopped()` on the Rust side.
 */
class MainActivity : AppCompatActivity() {

    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main)
    private var initialized = false

    private val appControl by lazy { AppControlClient(AppControlCodecsImpl()) }

    private val notificationPermissionLauncher =
        registerForActivityResult(RequestPermission()) { /* result ignored */ }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_main)

        if (!initialized) {
            val dispatcher = ServiceControlDispatcher(
                ServiceControlBackendImpl(applicationContext),
                ServiceControlCodecsImpl(),
            )
            IstmoRuntime.registerHandler(ServiceControlDispatcher.PLUGIN_ID, dispatcher)
            IstmoRuntime.start()
            initialized = true
        }

        maybeRequestNotificationPermission()
        wirePingSection()
        wireServiceSection()
    }

    override fun onDestroy() {
        scope.cancel()
        super.onDestroy()
    }

    private fun maybeRequestNotificationPermission() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU) return
        val granted = ContextCompat.checkSelfPermission(
            this,
            Manifest.permission.POST_NOTIFICATIONS,
        ) == PackageManager.PERMISSION_GRANTED
        if (!granted) {
            notificationPermissionLauncher.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    private fun wirePingSection() {
        val button = findViewById<Button>(R.id.pingButton)
        val output = findViewById<TextView>(R.id.pingOutput)
        button.setOnClickListener {
            scope.launch {
                output.text = try {
                    appControl.ping()
                } catch (e: AppControlException) {
                    "error: ${e.decoded?.reason ?: "unknown"}"
                } catch (t: Throwable) {
                    "transport error: ${t.message}"
                }
            }
        }
    }

    private fun wireServiceSection() {
        val startBtn = findViewById<Button>(R.id.startSync)
        val stopBtn = findViewById<Button>(R.id.stopSync)
        val output = findViewById<TextView>(R.id.serviceOutput)
        val intent = Intent(this, SyncService::class.java)

        startBtn.setOnClickListener {
            ContextCompat.startForegroundService(this, intent)
            output.text = "requested start"
        }
        stopBtn.setOnClickListener {
            stopService(intent)
            output.text = "requested stop"
        }
    }
}
