package dev.istmo.plugins.camera

import androidx.activity.ComponentActivity
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.CameraBackend
import dev.istmo.runtime.CameraError
import dev.istmo.runtime.CameraFactory
import dev.istmo.runtime.CameraInitConfig
import java.lang.ref.WeakReference

/**
 * Instantiates [CameraBackendImpl] for the host runtime. Registered
 * automatically by `IstmoPluginRegistry.registerAll(context, runtime)`
 * when the app enables `[app] auto_register` for the camera plugin.
 *
 * Holds the activity weakly: the registry registers a fresh factory on
 * every activity `onCreate`, so a destroyed activity must not stay
 * reachable through the previous one.
 */
class CameraFactoryImpl(activity: ComponentActivity) : CameraFactory {
    private val activityRef = WeakReference(activity)

    override suspend fun create(config: CameraInitConfig): CameraBackend {
        val activity = activityRef.get()
            ?: throw BackendException(CameraError.Backend("host activity is gone"))
        return CameraBackendImpl(activity, config)
    }
}
