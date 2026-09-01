package dev.istmo.demo

import android.app.Activity
import android.content.pm.PackageManager
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.PluginHandler
import java.io.ByteArrayOutputStream
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Kotlin backend for the `istmo.permissions` plugin.
 *
 * The plugin's on-wire methods are:
 * * `check(String) -> PermissionStatus`
 * * `request(Vec<String>) -> Vec<PermissionOutcome>`
 * * `should_show_rationale(String) -> Boolean`
 *
 * `PermissionStatus` variants map to their Rust ordinals as
 * `Granted=0, Denied=1, PermanentlyDenied=2, NotDetermined=3, NotSupported=4`.
 * `PermissionOutcome` is a bincode struct `{ permission: String, status:
 * PermissionStatus }`.
 *
 * The class does not hold the [Activity] directly — the surrounding UI
 * attaches it via [attach] on `onCreate` and detaches on `onDestroy`.
 */
class PermissionsImpl : PluginHandler {

    private var host: PermissionsHost? = null

    fun attach(host: PermissionsHost) {
        this.host = host
    }

    fun detach() {
        this.host = null
    }

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
            val outcomes = withContext(Dispatchers.Main) {
                requestAll(permissions)
            }
            encodeOutcomes(outcomes)
        }
        "should_show_rationale" -> {
            val (permission, _) = Bincode.readString(payload)
            val host = this.host
            val show = host?.shouldShowRationale(permission) ?: false
            val out = ByteArrayOutputStream(1)
            Bincode.writeBool(out, show)
            out.toByteArray()
        }
        else -> error("unknown Permissions method: $method")
    }

    private fun currentStatus(permission: String): PermissionStatusValue {
        val host = this.host ?: return PermissionStatusValue.NotDetermined
        val granted = ContextCompat.checkSelfPermission(host.activity, permission) ==
            PackageManager.PERMISSION_GRANTED
        return if (granted) PermissionStatusValue.Granted else PermissionStatusValue.Denied
    }

    private suspend fun requestAll(
        permissions: List<String>,
    ): List<Pair<String, PermissionStatusValue>> {
        val host = this.host
            ?: return permissions.map { it to PermissionStatusValue.NotSupported }
        val results: Map<String, Boolean> = suspendCoroutine { cont ->
            host.request(permissions) { cont.resume(it) }
        }
        return permissions.map { permission ->
            val granted = results[permission] ?: false
            val status = when {
                granted -> PermissionStatusValue.Granted
                host.shouldShowRationale(permission) -> PermissionStatusValue.Denied
                else -> PermissionStatusValue.PermanentlyDenied
            }
            permission to status
        }
    }

    private fun encodeStatus(status: PermissionStatusValue): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeEnumDiscriminant(out, status.ordinal)
        return out.toByteArray()
    }

    private fun encodeOutcomes(
        outcomes: List<Pair<String, PermissionStatusValue>>,
    ): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeVec(out, outcomes) { sink, outcome ->
            Bincode.writeString(sink, outcome.first)
            Bincode.writeEnumDiscriminant(sink, outcome.second.ordinal)
        }
        return out.toByteArray()
    }
}

/** Mirror of Rust `istmo::plugins::PermissionStatus`. Ordinals are the wire discriminant. */
enum class PermissionStatusValue {
    Granted,
    Denied,
    PermanentlyDenied,
    NotDetermined,
    NotSupported,
}

/** Hookup surface [PermissionsImpl] uses to touch the Activity subsystem. */
interface PermissionsHost {
    val activity: Activity

    /**
     * Kicks off a permission request. `onResult` receives one entry per input
     * permission with a boolean granted/denied outcome.
     */
    fun request(permissions: List<String>, onResult: (Map<String, Boolean>) -> Unit)

    fun shouldShowRationale(permission: String): Boolean =
        ActivityCompat.shouldShowRequestPermissionRationale(activity, permission)
}
