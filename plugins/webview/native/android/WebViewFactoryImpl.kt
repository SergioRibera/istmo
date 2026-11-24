package dev.istmo.plugins.webview

import android.app.Activity
import androidx.activity.ComponentActivity
import dev.istmo.runtime.BackendException
import dev.istmo.runtime.IstmoActivityBound
import java.lang.ref.WeakReference

/**
 * Builds [WebViewBackendImpl]s. Created once per process
 * ([IstmoActivityBound]); every new activity is handed to the live
 * backends, which move their views onto it.
 */
class WebViewFactoryImpl(activity: ComponentActivity) : WebViewFactory, IstmoActivityBound {
    @Volatile
    private var activityRef = WeakReference(activity)

    private val backends = ArrayList<WeakReference<WebViewBackendImpl>>()

    override fun attachActivity(activity: Activity) {
        val host = activity as ComponentActivity
        activityRef = WeakReference(host)
        val live = synchronized(backends) {
            backends.removeAll { it.get() == null }
            backends.mapNotNull { it.get() }
        }
        live.forEach { it.attachActivity(host) }
    }

    override suspend fun create(config: WebViewInitConfig): WebViewBackend {
        val activity = activityRef.get()
            ?: throw BackendException(WebViewError.Backend("host activity is gone"))
        val backend = WebViewBackendImpl(activity, config)
        synchronized(backends) {
            backends.removeAll { it.get() == null }
            backends.add(WeakReference(backend))
        }
        return backend
    }
}
