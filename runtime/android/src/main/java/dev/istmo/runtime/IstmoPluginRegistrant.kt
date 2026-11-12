package dev.istmo.runtime

import android.app.Activity
import android.content.Context

/**
 * Implemented by the `dev.istmo.generated.IstmoPluginRegistry` object that
 * `istmo-build` generates into every app: registers each auto-registered
 * plugin dispatcher with [IstmoRuntime].
 *
 * Handlers outlive any single activity, so context-bound backends are
 * built with the application context. Backends that need the activity
 * are registered through [registerActivityPlugins] and named in
 * [activityPluginIds], so [IstmoHost] can rebind or drop them when that
 * activity goes away instead of keeping it reachable.
 */
interface IstmoPluginRegistrant {
    /**
     * Register every plugin. Activity-bound plugins are registered only
     * when [context] is an activity, and skipped otherwise (for example
     * when registering from a `Service`).
     */
    fun registerAll(context: Context)

    /** Ids of the plugins whose backend holds the host activity. */
    val activityPluginIds: List<String>
        get() = emptyList()

    /** Register, or re-register, only the activity-bound plugins with [activity]. */
    fun registerActivityPlugins(activity: Activity) {}
}
