package dev.istmo.plugins.camera

import androidx.activity.ComponentActivity
import dev.istmo.runtime.CameraBackend
import dev.istmo.runtime.CameraFactory
import dev.istmo.runtime.CameraInitConfig

/**
 * Instantiates [CameraBackendImpl] for the host runtime. Registered
 * automatically by `IstmoPluginRegistry.registerAll(context, runtime)`
 * when the app enables `[app] auto_register` for the camera plugin.
 */
class CameraFactoryImpl(private val activity: ComponentActivity) : CameraFactory {
    override suspend fun create(config: CameraInitConfig): CameraBackend =
        CameraBackendImpl(activity, config)
}
