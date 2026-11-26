@file:Suppress("UNUSED_PARAMETER", "unused")

package android.util

class DisplayMetrics {
    @JvmField var widthPixels: Int = 0
    @JvmField var heightPixels: Int = 0
    @JvmField var densityDpi: Int = 0

    companion object {
        const val DENSITY_DEFAULT: Int = 160
    }
}

class Log {
    companion object {
        @JvmStatic
        fun w(tag: String, msg: String): Int = 0

        @JvmStatic
        fun w(tag: String, msg: String, tr: Throwable): Int = 0
    }
}
