@file:Suppress("UNUSED_PARAMETER", "unused")

package android.media

open class Image : AutoCloseable {
    val hardwareBuffer: android.hardware.HardwareBuffer? get() = null
    val fence: android.hardware.SyncFence get() = android.hardware.SyncFence()
    val timestamp: Long get() = 0L
    val width: Int get() = 0
    val height: Int get() = 0
    val planes: Array<Plane> get() = emptyArray()
    override fun close() {}

    open class Plane {
        val buffer: java.nio.ByteBuffer get() = java.nio.ByteBuffer.allocateDirect(0)
        val rowStride: Int get() = 0
        val pixelStride: Int get() = 0
    }
}
