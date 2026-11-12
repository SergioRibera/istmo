package dev.istmo.runtime

import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.util.Log
import java.lang.ref.WeakReference

/**
 * Activity-side lifecycle of an istmo app: loads the Rust library, starts
 * [IstmoRuntime] once per process and registers every plugin through the
 * generated registry.
 *
 * [IstmoGameActivity] and [IstmoNativeActivity] call it for you. An app
 * with its own activity calls [onCreate] before `super.onCreate` (the Rust
 * entry point may acquire plugin clients as soon as it runs) and
 * [onDestroy] from `onDestroy`.
 *
 * Plugin handlers are process-wide, so only plugins whose backend needs
 * an activity are tied to one: they follow the most recently created
 * live activity and are unregistered once none is left, which keeps a
 * finished activity from staying reachable through the runtime.
 */
object IstmoHost {

    /** Fully-qualified name of the registry `istmo-build` generates. */
    const val REGISTRY_CLASS = "dev.istmo.generated.IstmoPluginRegistry"

    /** Activity meta-data naming the Rust library, shared with `NativeActivity` / `GameActivity`. */
    const val LIB_NAME_META_DATA = "android.app.lib_name"

    private const val TAG = "istmo"

    private var started = false
    private var builtinObservers: IstmoBuiltinObservers? = null

    /** Activities between [onCreate] and [onDestroy], oldest first; observers stop when empty. */
    private val liveActivities = ArrayList<WeakReference<Activity>>()

    /** Set once [IstmoPluginRegistrant.registerAll] ran in this process. */
    private var pluginsRegistered = false

    /** Activity the activity-bound plugins are currently registered with. */
    private var pluginActivity: WeakReference<Activity>? = null

    private var registrant: IstmoPluginRegistrant? = null
    private var registrantLookedUp = false

    /**
     * Start the runtime (first call per process only) and register every
     * plugin with [activity] as host. The library name comes from the
     * activity's `android.app.lib_name` meta-data unless [libraryName] is
     * given.
     *
     * Context-bound plugins are registered once per process; activity-bound
     * ones are re-registered with every new activity.
     *
     * Also wires [IstmoBuiltinObservers] so wire-only system plugins
     * ([dev.istmo.runtime] system-info / theme / network / battery…)
     * start publishing without the app having to opt in by hand.
     */
    @JvmStatic
    @JvmOverloads
    fun onCreate(activity: Activity, libraryName: String? = null) {
        val registerAll: Boolean
        synchronized(this) {
            if (!started) {
                val lib = libraryName ?: libraryName(activity)
                IstmoRuntime.start(lib)
                started = true
            }
            liveActivities.add(WeakReference(activity))
            registerAll = !pluginsRegistered
        }
        if (registerAll) {
            registerPlugins(activity)
        } else {
            bindActivityPlugins(activity)
        }
        registerBuiltinObservers(activity.applicationContext)
    }

    /**
     * Release what [activity] holds: activity-bound plugins move to the
     * most recent activity still alive, or are unregistered when there is
     * none (a configuration change leaves them to the recreated activity).
     *
     * Builtin observers stop once the last istmo activity is finishing for
     * good; another activity of the app still running keeps them
     * registered.
     *
     * The runtime itself stays up: it is process-scoped and cannot be
     * started again after [IstmoRuntime.shutdown], while Android may
     * relaunch the activity in the same process. The OS reclaims it with
     * the process.
     */
    @JvmStatic
    fun onDestroy(activity: Activity) {
        var rebindTo: Activity? = null
        var dropActivityPlugins = false
        synchronized(this) {
            // Count every destroy, configuration changes included: the
            // recreated activity counts itself again in onCreate.
            liveActivities.removeAll { ref -> ref.get().let { it == null || it === activity } }
            if (pluginActivity?.get() === activity && !activity.isChangingConfigurations) {
                rebindTo = liveActivities
                    .asReversed()
                    .firstNotNullOfOrNull { ref -> ref.get()?.takeUnless { it.isFinishing || it.isDestroyed } }
                if (rebindTo == null) {
                    pluginActivity = null
                    dropActivityPlugins = true
                }
            }
            if (liveActivities.isEmpty() && activity.isFinishing && !activity.isChangingConfigurations) {
                builtinObservers?.unregisterAll()
                builtinObservers = null
            }
        }
        rebindTo?.let(::bindActivityPlugins)
        if (dropActivityPlugins) {
            registrant()?.activityPluginIds?.forEach(IstmoRuntime::unregisterHandler)
        }
    }

    /**
     * Wire [IstmoBuiltinObservers] so wire-only system plugins publish
     * from this process. Called automatically from [onCreate]; exposed
     * for apps that drive runtime startup themselves.
     */
    @JvmStatic
    fun registerBuiltinObservers(context: Context) {
        synchronized(this) {
            if (builtinObservers != null) return
            val observers = IstmoBuiltinObservers(context.applicationContext)
            observers.registerAll()
            builtinObservers = observers
        }
    }

    /**
     * Register every plugin through the generated registry. Returns `false`
     * when the app has no registry (it does not run `istmo-build`).
     *
     * Activity-bound plugins are registered only when [context] is an
     * activity; that activity then hosts them until [onDestroy] moves them.
     */
    @JvmStatic
    fun registerPlugins(context: Context): Boolean {
        val registrant = registrant() ?: return false
        registrant.registerAll(context)
        synchronized(this) {
            pluginsRegistered = true
            if (context is Activity) pluginActivity = WeakReference(context)
        }
        return true
    }

    private fun bindActivityPlugins(activity: Activity) {
        val registrant = registrant() ?: return
        if (registrant.activityPluginIds.isEmpty()) return
        registrant.registerActivityPlugins(activity)
        synchronized(this) { pluginActivity = WeakReference(activity) }
    }

    private fun registrant(): IstmoPluginRegistrant? = synchronized(this) {
        if (!registrantLookedUp) {
            registrantLookedUp = true
            registrant = try {
                Class.forName(REGISTRY_CLASS).getField("INSTANCE").get(null) as IstmoPluginRegistrant
            } catch (_: ClassNotFoundException) {
                Log.w(TAG, "$REGISTRY_CLASS not found; register plugin dispatchers manually")
                null
            }
        }
        registrant
    }

    private fun libraryName(activity: Activity): String {
        @Suppress("DEPRECATION")
        val info = activity.packageManager.getActivityInfo(activity.componentName, PackageManager.GET_META_DATA)
        return info.metaData?.getString(LIB_NAME_META_DATA)
            ?: throw IllegalStateException(
                "istmo: ${activity.javaClass.name} has no <meta-data android:name=\"$LIB_NAME_META_DATA\"> " +
                    "naming the Rust library",
            )
    }
}
