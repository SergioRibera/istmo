@file:Suppress("UNUSED_PARAMETER", "unused")

package android.content.pm

open class PackageManager {
    open fun getPackageUid(packageName: String, flags: Int): Int = 0

    class NameNotFoundException : Exception()

    companion object {
        const val PERMISSION_GRANTED: Int = 0
        const val GET_META_DATA: Int = 128
    }
}

open class ApplicationInfo {
    @JvmField var icon: Int = 0
    open fun loadLabel(pm: PackageManager): CharSequence = ""
}
