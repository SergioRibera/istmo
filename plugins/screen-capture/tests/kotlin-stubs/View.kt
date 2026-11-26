@file:Suppress("UNUSED_PARAMETER", "unused")

package android.view

open class Display {
    val displayId: Int get() = 0
    val name: String? get() = null
    val refreshRate: Float get() = 60f
    val rotation: Int get() = 0
    val flags: Int get() = 0
    fun getRealMetrics(outMetrics: android.util.DisplayMetrics) {}

    companion object {
        const val DEFAULT_DISPLAY: Int = 0
        const val FLAG_SECURE: Int = 2
    }
}

open class Surface(surfaceTexture: android.graphics.SurfaceTexture) {
    val isValid: Boolean get() = false
    fun release() {}

    companion object {
        const val ROTATION_0: Int = 0
        const val ROTATION_90: Int = 1
        const val ROTATION_180: Int = 2
        const val ROTATION_270: Int = 3
    }
}
