@file:Suppress("UNUSED_PARAMETER", "unused")

package android.media

open class Image : AutoCloseable {
    val hardwareBuffer: android.hardware.HardwareBuffer? get() = null
    val fence: android.hardware.SyncFence get() = android.hardware.SyncFence()
    override fun close() {}
}
