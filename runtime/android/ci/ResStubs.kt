// Stand-in for the resource lookup plugin backends use (a plugin's own
// `native/android/res`, merged into the app, has no `R` class it can name).
@file:Suppress("UNUSED_PARAMETER", "unused")

package android.content.res

open class Resources {
    open fun getIdentifier(name: String, defType: String, defPackage: String): Int = 0
}
