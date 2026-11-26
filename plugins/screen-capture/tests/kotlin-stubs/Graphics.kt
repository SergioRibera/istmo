@file:Suppress("UNUSED_PARAMETER", "unused")

package android.graphics

class PixelFormat {
    companion object {
        const val RGBA_8888: Int = 1
    }
}

class ImageFormat {
    companion object {
        const val PRIVATE: Int = 0x22
    }
}

open class SurfaceTexture(texName: Int) {
    fun interface OnFrameAvailableListener {
        fun onFrameAvailable(surfaceTexture: SurfaceTexture)
    }

    val timestamp: Long get() = 0L
    fun setOnFrameAvailableListener(listener: OnFrameAvailableListener?) {}
    fun setOnFrameAvailableListener(listener: OnFrameAvailableListener?, handler: android.os.Handler?) {}
    fun setDefaultBufferSize(width: Int, height: Int) {}
    fun updateTexImage() {}
    fun getTransformMatrix(mtx: FloatArray) {}
    fun release() {}
}
