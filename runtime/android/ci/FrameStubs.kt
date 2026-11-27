// Stand-ins for the Android SDK types `IstmoFrameSender` names, so it
// compiles on a plain JVM.
@file:Suppress("UNUSED_PARAMETER", "unused")

package android.hardware

open class HardwareBuffer : AutoCloseable {
    val isClosed: Boolean get() = false
    val format: Int get() = 0
    val id: Long get() = 0L
    override fun close() {}

    companion object {
        const val RGBA_8888: Int = 1
        const val RGBX_8888: Int = 2
        const val USAGE_GPU_SAMPLED_IMAGE: Long = 256L
        const val USAGE_GPU_COLOR_OUTPUT: Long = 512L

        fun isSupported(width: Int, height: Int, format: Int, layers: Int, usage: Long): Boolean = true
    }
}

class SyncFence : AutoCloseable {
    val isValid: Boolean get() = false
    fun await(timeout: java.time.Duration): Boolean = true
    override fun close() {}
}
