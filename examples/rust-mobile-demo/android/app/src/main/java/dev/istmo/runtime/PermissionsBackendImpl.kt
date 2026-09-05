package dev.istmo.runtime

import android.app.Activity
import android.content.pm.PackageManager
import android.os.Build
import androidx.core.app.ActivityCompat
import androidx.core.content.ContextCompat
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.resume
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Android impl of the codegen `PermissionsBackend` interface. The
 * `notifyPermissionsResult` entry point stays on this class so
 * [dev.istmo.rustdemo.RustMobileActivity.onRequestPermissionsResult] can
 * forward the OS callback into the suspended `request` coroutine.
 */
class PermissionsBackendImpl(private val activity: Activity) : PermissionsBackend {

    private val nextRequestCode = AtomicInteger(1)
    private val pending =
        ConcurrentHashMap<Int, (Map<String, Boolean>) -> Unit>()

    override suspend fun check(permission: String): PermissionStatus = currentStatus(permission)

    override suspend fun request(permissions: List<String>): List<PermissionOutcome> {
        val results = withContext(Dispatchers.Main) { requestAll(permissions) }
        return results.map { PermissionOutcome(it.first, it.second) }
    }

    override suspend fun should_show_rationale(permission: String): Boolean =
        ActivityCompat.shouldShowRequestPermissionRationale(activity, permission)

    /**
     * Route from `RustMobileActivity.onRequestPermissionsResult` into
     * the suspended request coroutine that fired the request.
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

    private fun currentStatus(permission: String): PermissionStatus {
        val granted = ContextCompat.checkSelfPermission(activity, permission) ==
            PackageManager.PERMISSION_GRANTED
        return if (granted) PermissionStatus.Granted else PermissionStatus.Denied
    }

    private suspend fun requestAll(
        permissions: List<String>,
    ): List<Pair<String, PermissionStatus>> {
        val stillNeeded = permissions.filter { currentStatus(it) != PermissionStatus.Granted }
        if (stillNeeded.isEmpty()) {
            return permissions.map { it to PermissionStatus.Granted }
        }
        // Android <13 has no runtime gate for POST_NOTIFICATIONS — treat as granted.
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU &&
            stillNeeded.all { it == "android.permission.POST_NOTIFICATIONS" }
        ) {
            return permissions.map { it to PermissionStatus.Granted }
        }

        val code = nextRequestCode.getAndIncrement()
        val results: Map<String, Boolean> = suspendCoroutine { cont ->
            pending[code] = { cont.resume(it) }
            ActivityCompat.requestPermissions(activity, stillNeeded.toTypedArray(), code)
        }
        return permissions.map { permission ->
            val granted = results[permission]
                ?: (currentStatus(permission) == PermissionStatus.Granted)
            val status = when {
                granted -> PermissionStatus.Granted
                ActivityCompat.shouldShowRequestPermissionRationale(activity, permission) ->
                    PermissionStatus.Denied
                else -> PermissionStatus.PermanentlyDenied
            }
            permission to status
        }
    }
}
