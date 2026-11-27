@file:Suppress("UNUSED_PARAMETER", "unused")

package android.media

open class ImageReader {
    fun interface OnImageAvailableListener {
        fun onImageAvailable(reader: ImageReader)
    }

    val surface: android.view.Surface get() = throw UnsupportedOperationException()
    fun setOnImageAvailableListener(listener: OnImageAvailableListener?, handler: android.os.Handler?) {}
    fun acquireNextImage(): Image? = null
    fun acquireLatestImage(): Image? = null
    fun close() {}

    companion object {
        @JvmStatic
        fun newInstance(width: Int, height: Int, format: Int, maxImages: Int): ImageReader = ImageReader()

        @JvmStatic
        fun newInstance(width: Int, height: Int, format: Int, maxImages: Int, usage: Long): ImageReader = ImageReader()
    }
}

class AudioAttributes {
    companion object {
        const val USAGE_UNKNOWN: Int = 0
        const val USAGE_MEDIA: Int = 1
        const val USAGE_GAME: Int = 14
    }
}

class AudioFormat {
    class Builder {
        fun setEncoding(encoding: Int): Builder = this
        fun setSampleRate(sampleRate: Int): Builder = this
        fun setChannelMask(channelMask: Int): Builder = this
        fun build(): AudioFormat = AudioFormat()
    }

    companion object {
        const val ENCODING_PCM_16BIT: Int = 2
        const val ENCODING_PCM_FLOAT: Int = 4
        const val CHANNEL_IN_MONO: Int = 16
        const val CHANNEL_IN_STEREO: Int = 12
    }
}

class AudioPlaybackCaptureConfiguration {
    class Builder(projection: android.media.projection.MediaProjection) {
        fun addMatchingUsage(usage: Int): Builder = this
        fun addMatchingUid(uid: Int): Builder = this
        fun excludeUid(uid: Int): Builder = this
        fun build(): AudioPlaybackCaptureConfiguration = AudioPlaybackCaptureConfiguration()
    }
}

class AudioTimestamp {
    @JvmField var framePosition: Long = 0L
    @JvmField var nanoTime: Long = 0L

    companion object {
        const val TIMEBASE_MONOTONIC: Int = 0
    }
}

open class AudioRecord {
    class Builder {
        fun setAudioFormat(format: AudioFormat): Builder = this
        fun setBufferSizeInBytes(bufferSizeInBytes: Int): Builder = this
        fun setAudioPlaybackCaptureConfig(config: AudioPlaybackCaptureConfiguration): Builder = this
        fun build(): AudioRecord = AudioRecord()
    }

    val state: Int get() = 0
    val recordingState: Int get() = 0
    fun startRecording() {}
    fun read(audioBuffer: java.nio.ByteBuffer, sizeInBytes: Int, readMode: Int): Int = 0
    fun getTimestamp(outTimestamp: AudioTimestamp, timebase: Int): Int = 0
    fun stop() {}
    fun release() {}

    companion object {
        const val STATE_INITIALIZED: Int = 1
        const val RECORDSTATE_RECORDING: Int = 3
        const val READ_BLOCKING: Int = 0
        const val SUCCESS: Int = 0

        @JvmStatic
        fun getMinBufferSize(sampleRateInHz: Int, channelConfig: Int, audioFormat: Int): Int = 0
    }
}
