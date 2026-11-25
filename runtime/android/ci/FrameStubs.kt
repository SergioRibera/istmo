// Stand-ins for the Android SDK types `IstmoFrameSender` names, so it
// compiles on a plain JVM.
@file:Suppress("UNUSED_PARAMETER", "unused")

package android.hardware

open class HardwareBuffer : AutoCloseable {
    val isClosed: Boolean get() = false
    override fun close() {}
}

class SyncFence : AutoCloseable {
    val isValid: Boolean get() = false
    fun await(timeout: java.time.Duration): Boolean = true
    override fun close() {}
}
