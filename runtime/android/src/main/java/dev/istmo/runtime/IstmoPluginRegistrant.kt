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
 * go through [registerActivityPlugins] and [releaseActivityPlugins], so
 * [IstmoHost] can move them to a new activity or drop them instead of
 * keeping a finished one reachable.
 */
interface IstmoPluginRegistrant {
    /**
     * Register every plugin. Activity-bound plugins are registered only
     * when [context] is an activity, and skipped otherwise (for example
     * when registering from a `Service`).
     */
    fun registerAll(context: Context)

    /**
     * Give [activity] to the activity-bound plugins: an [IstmoActivityBound]
     * backend already registered is handed it, any other is built and
     * registered anew.
     */
    fun registerActivityPlugins(activity: Activity) {}

    /**
     * No activity is left: unregister the activity-bound backends that
     * would otherwise keep the last one alive. [IstmoActivityBound]
     * backends hold it weakly and stay registered.
     */
    fun releaseActivityPlugins() {}
}
