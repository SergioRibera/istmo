package dev.istmo.runtime

import android.app.Activity
import android.content.Context
import android.content.pm.PackageManager
import android.util.Log

/**
 * Activity-side lifecycle of an istmo app: loads the Rust library, starts
 * [IstmoRuntime] once per process and registers every plugin through the
 * generated registry.
 *
 * [IstmoGameActivity] and [IstmoNativeActivity] call it for you. An app
 * with its own activity calls [onCreate] before `super.onCreate` (the Rust
 * entry point may acquire plugin clients as soon as it runs) and
 * [onDestroy] from `onDestroy`.
 */
object IstmoHost {

    /** Fully-qualified name of the registry `istmo-build` generates. */
    const val REGISTRY_CLASS = "dev.istmo.generated.IstmoPluginRegistry"

    /** Activity meta-data naming the Rust library, shared with `NativeActivity` / `GameActivity`. */
    const val LIB_NAME_META_DATA = "android.app.lib_name"

    private const val TAG = "istmo"

    private var started = false

    /**
     * Start the runtime (first call per process only) and register every
     * plugin with [activity] as host. The library name comes from the
     * activity's `android.app.lib_name` meta-data unless [libraryName] is
     * given.
     */
    @JvmStatic
    @JvmOverloads
    fun onCreate(activity: Activity, libraryName: String? = null) {
        synchronized(this) {
            if (!started) {
                val lib = libraryName ?: libraryName(activity)
                check(IstmoRuntime.start(lib)) { "istmo: IstmoRuntime.start(\"$lib\") failed" }
                started = true
            }
        }
        registerPlugins(activity)
    }

    /** Shut the runtime down once [activity] is finishing for good. */
    @JvmStatic
    fun onDestroy(activity: Activity) {
        if (!activity.isFinishing || activity.isChangingConfigurations) return
        synchronized(this) {
            if (started) {
                IstmoRuntime.shutdown()
                started = false
            }
        }
    }

    /**
     * Register every plugin through the generated registry. Returns `false`
     * when the app has no registry (it does not run `istmo-build`).
     */
    @JvmStatic
    fun registerPlugins(context: Context): Boolean {
        val registrant = try {
            Class.forName(REGISTRY_CLASS).getField("INSTANCE").get(null) as IstmoPluginRegistrant
        } catch (_: ClassNotFoundException) {
            Log.w(TAG, "$REGISTRY_CLASS not found; register plugin dispatchers manually")
            return false
        }
        registrant.registerAll(context)
        return true
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
