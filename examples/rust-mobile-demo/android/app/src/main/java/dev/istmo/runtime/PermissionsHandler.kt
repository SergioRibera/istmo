package dev.istmo.runtime

import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Kotlin backend for the `istmo.permissions` plugin.
 *
 * Wire methods:
 *  * `check(String) -> PermissionStatus` — synchronous status query.
 *  * `request(Vec<String>) -> Vec<PermissionOutcome>` — prompt for a set.
 *  * `should_show_rationale(String) -> Boolean`.
 *
 * The rationale flow requires the OS to hand us
 * `onRequestPermissionsResult` back. Since we live inside a
 * [NativeActivity] subclass we override that callback in
 * [RustMobileActivity] and forward the results to
 * [notifyPermissionsResult] here.
 */
class PermissionsHandler(private val activity: Activity) : PluginHandler {

    companion object {
        const val PLUGIN_ID = "istmo.permissions"
    }

    private val nextRequestCode = AtomicInteger(1)
    private val pending =
        ConcurrentHashMap<Int, (Map<String, Boolean>) -> Unit>()

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "check" -> {
            val (permission, _) = Bincode.readString(payload)
            encodeStatus(currentStatus(permission))
        }
        "request" -> {
            val (permissions, _) = Bincode.readVec(payload, 0, Bincode::readString)
            val results = withContext(Dispatchers.Main) { requestAll(permissions) }
            encodeOutcomes(results)
        }
        "should_show_rationale" -> {
            val (permission, _) = Bincode.readString(payload)
            val out = ByteArrayOutputStream(1)
            Bincode.writeBool(
                out,
                ActivityCompat.shouldShowRequestPermissionRationale(activity, permission),
            )
            out.toByteArray()
        }
        else -> error("unknown Permissions method: $method")
    }

    /**
     * Route from [RustMobileActivity.onRequestPermissionsResult] into the
     * suspended request coroutine that fired the request.
     */
    fun notifyPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray,
    ) {
        val callback = pending.remove(requestCode) ?: return
        val map = HashMap<String, Boolean>(permissions.size)
        for (i in permissions.indices) {
            val granted = grantResults.getOrNull(i) == PackageManager.PERMISSION_GRANTED
            map[permissions[i]] = granted
        }
        callback(map)
    }

    private fun currentStatus(permission: String): Status {
        val granted = ContextCompat.checkSelfPermission(activity, permission) ==
            PackageManager.PERMISSION_GRANTED
        return if (granted) Status.Granted else Status.Denied
    }

    private suspend fun requestAll(
        permissions: List<String>,
    ): List<Pair<String, Status>> {
        // Fast-path: everything already granted.
        val stillNeeded = permissions.filter { currentStatus(it) != Status.Granted }
        if (stillNeeded.isEmpty()) {
            return permissions.map { it to Status.Granted }
        }
        // Android <13 has no runtime gate for POST_NOTIFICATIONS — treat as granted.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU &&
            stillNeeded.all { it == "android.permission.POST_NOTIFICATIONS" }
        ) {
            return permissions.map { it to Status.Granted }
        }

        val code = nextRequestCode.getAndIncrement()
        val results: Map<String, Boolean> = suspendCoroutine { cont ->
            pending[code] = { cont.resume(it) }
            ActivityCompat.requestPermissions(activity, stillNeeded.toTypedArray(), code)
        }
        return permissions.map { permission ->
            val granted = results[permission]
                ?: (currentStatus(permission) == Status.Granted)
            val status = when {
                granted -> Status.Granted
                ActivityCompat.shouldShowRequestPermissionRationale(activity, permission) ->
                    Status.Denied
                else -> Status.PermanentlyDenied
            }
            permission to status
        }
    }

    private fun encodeStatus(status: Status): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeEnumDiscriminant(out, status.ordinal)
        return out.toByteArray()
    }

    private fun encodeOutcomes(outcomes: List<Pair<String, Status>>): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeVec(out, outcomes) { sink, outcome ->
            Bincode.writeString(sink, outcome.first)
            Bincode.writeEnumDiscriminant(sink, outcome.second.ordinal)
        }
        return out.toByteArray()
    }

    /** Mirrors Rust `istmo::plugins::PermissionStatus` variant order. */
    private enum class Status {
        Granted,
        Denied,
        PermanentlyDenied,
        NotDetermined,
        NotSupported,
    }
}
