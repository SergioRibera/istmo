@file:Suppress("UNUSED_PARAMETER", "unused")

package android.hardware.display

open class DisplayManager {
    interface DisplayListener {
        fun onDisplayAdded(displayId: Int)
        fun onDisplayRemoved(displayId: Int)
        fun onDisplayChanged(displayId: Int)
    }

    val displays: Array<android.view.Display> get() = emptyArray()
    fun getDisplay(displayId: Int): android.view.Display? = null
    fun registerDisplayListener(listener: DisplayListener, handler: android.os.Handler?) {}
    fun unregisterDisplayListener(listener: DisplayListener) {}

    companion object {
        const val VIRTUAL_DISPLAY_FLAG_AUTO_MIRROR: Int = 16
    }
}

open class VirtualDisplay {
    abstract class Callback

    var surface: android.view.Surface? = null
    fun resize(width: Int, height: Int, densityDpi: Int) {}
    fun release() {}
}
