package dev.istmo.plugins.camera

import android.app.Activity
import androidx.activity.ComponentActivity
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import java.lang.ref.WeakReference

/**
 * Instantiates [CameraBackendImpl] for the host runtime. Registered
 * automatically by `IstmoPluginRegistry.registerAll(context, runtime)`
 * when the app enables `[app] auto_register` for the camera plugin.
 *
 * Built once per process ([IstmoActivityBound]): the registry hands it
 * every new activity, which it passes on to the backends it created, so
 * camera instances survive activity recreation. Activities are held
 * weakly, so a destroyed one never stays reachable from here.
 */
class CameraFactoryImpl(activity: ComponentActivity) : CameraFactory, IstmoActivityBound {
    @Volatile
    private var activityRef = WeakReference(activity)

    /** Backends handed out so far; dead references are pruned on use. */
    private val backends = ArrayList<WeakReference<CameraBackendImpl>>()

    override fun attachActivity(activity: Activity) {
        // The registry only hands over activities of the declared class.
        val host = activity as ComponentActivity
        activityRef = WeakReference(host)
        val live = synchronized(backends) {
            backends.removeAll { it.get() == null }
            backends.mapNotNull { it.get() }
        }
        live.forEach { it.attachActivity(host) }
    }

    override suspend fun create(config: CameraInitConfig): CameraBackend {
        val activity = activityRef.get()
            ?: throw BackendException(CameraError.Backend("host activity is gone"))
        val backend = CameraBackendImpl(activity, config)
        synchronized(backends) {
            backends.removeAll { it.get() == null }
            backends.add(WeakReference(backend))
        }
        return backend
    }
}
