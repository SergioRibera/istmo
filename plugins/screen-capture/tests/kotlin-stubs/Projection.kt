@file:Suppress("UNUSED_PARAMETER", "unused")

package android.media.projection

open class MediaProjection {
    abstract class Callback {
        open fun onStop() {}
        open fun onCapturedContentResize(width: Int, height: Int) {}
        open fun onCapturedContentVisibilityChanged(isVisible: Boolean) {}
    }

    fun registerCallback(callback: Callback, handler: android.os.Handler?) {}
    fun stop() {}
    fun createVirtualDisplay(
        name: String,
        width: Int,
        height: Int,
        dpi: Int,
        flags: Int,
        surface: android.view.Surface?,
        callback: android.hardware.display.VirtualDisplay.Callback?,
        handler: android.os.Handler?,
    ): android.hardware.display.VirtualDisplay? = null
}

class MediaProjectionConfig {
    companion object {
        @JvmStatic
        fun createConfigForDefaultDisplay(): MediaProjectionConfig = MediaProjectionConfig()

        @JvmStatic
        fun createConfigForUserChoice(): MediaProjectionConfig = MediaProjectionConfig()
    }
}

open class MediaProjectionManager {
    fun createScreenCaptureIntent(): android.content.Intent = android.content.Intent()
    fun createScreenCaptureIntent(config: MediaProjectionConfig): android.content.Intent = android.content.Intent()
    fun getMediaProjection(resultCode: Int, resultData: android.content.Intent): MediaProjection? = null
}
