// Minimal stand-ins for the Android / AndroidX surface the generated
// services and workers, and the plugin backends checked by
// `scripts/check-plugin-native.sh`, use, so they compile on a plain JVM.
@file:Suppress("UNUSED_PARAMETER", "unused")

package android.content

open class Context {
    open val applicationContext: Context get() = this
    open val packageName: String get() = ""
    open val packageManager: android.content.pm.PackageManager get() = android.content.pm.PackageManager()
    open val applicationInfo: android.content.pm.ApplicationInfo get() = android.content.pm.ApplicationInfo()
    open val resources: android.content.res.Resources get() = android.content.res.Resources()
    open fun getSystemService(name: String): Any? = null
    open fun checkSelfPermission(permission: String): Int = -1
    open fun getSharedPreferences(name: String, mode: Int): SharedPreferences = SharedPreferences()
    open fun startService(service: Intent): ComponentName? = null
    open fun startForegroundService(service: Intent): ComponentName? = null
    open fun stopService(service: Intent): Boolean = false

    companion object {
        const val MODE_PRIVATE: Int = 0
        const val DISPLAY_SERVICE: String = "display"
        const val MEDIA_PROJECTION_SERVICE: String = "media_projection"
        const val NOTIFICATION_SERVICE: String = "notification"
    }
}

class ComponentName

open class Intent() {
    private var intentAction: String? = null

    constructor(context: Context, cls: Class<*>) : this()

    val action: String? get() = intentAction

    fun setAction(action: String?): Intent {
        intentAction = action
        return this
    }
}

open class SharedPreferences {
    open fun getBoolean(key: String, defValue: Boolean): Boolean = defValue
    open fun edit(): Editor = Editor()

    open class Editor {
        open fun putBoolean(key: String, value: Boolean): Editor = this
        open fun apply() {}
    }
}
