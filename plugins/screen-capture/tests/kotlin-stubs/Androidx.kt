@file:Suppress("UNUSED_PARAMETER", "unused")

package androidx.activity

open class ComponentActivity : android.app.Activity() {
    val activityResultRegistry: androidx.activity.result.ActivityResultRegistry =
        androidx.activity.result.ActivityResultRegistry()
}
