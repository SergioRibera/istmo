package dev.istmo.plugins.screencapture

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.media.AudioAttributes
import android.media.AudioFormat
import android.media.AudioPlaybackCaptureConfiguration
import android.media.AudioRecord
import android.media.AudioTimestamp
import android.media.projection.MediaProjection
import android.os.Build
import android.os.Process
import android.util.Log
import dev.istmo.runtime.IstmoFrameSender
import java.nio.ByteBuffer
import kotlin.math.abs

/**
 * What an `AudioConfig` asks for, checked before any consent is shown.
 */
internal class AudioPlan(val config: AudioConfig, val rate: Int, val channels: Int) {
    val negotiated: AudioNegotiated
        get() = AudioNegotiated(config.source, rate.toUInt(), channels.toUByte(), config.format)

    val bytesPerFrame: Int get() = channels * if (config.format == SampleFormat.F32) 4 else 2

    companion object {
        private const val DEFAULT_RATE = 48_000
        private const val DEFAULT_CHANNELS = 2

        /**
         * The plan, or the requirement it misses: playback capture is API 29+
         * and needs `RECORD_AUDIO`; PipeWire nodes do not exist here.
         */
        fun of(context: Context, config: AudioConfig, selection: Selection): AudioPlan {
            if (Build.VERSION.SDK_INT < Api.Q) fail(CaptureError.OsTooOld(OsVersion(10u, 0u, 0u)))
            if (config.source is AudioSource.Node) selection.unsatisfiable(Requirement.Audio)
            val rate = config.rate?.toInt() ?: DEFAULT_RATE
            val channels = config.channels?.toInt() ?: DEFAULT_CHANNELS
            if (rate !in 8_000..192_000 || channels !in 1..2) selection.unsatisfiable(Requirement.Audio)
            if (context.checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
                fail(CaptureError.BackendUnavailable(BackendReason(PROJECTION, Availability.NeedsAccess(AccessScope.Microphone))))
            }
            if (config.source is AudioSource.Application) uidOf(context, config.source.value)
            return AudioPlan(config, rate, channels)
        }

        fun uidOf(context: Context, packageName: String): Int = try {
            context.packageManager.getPackageUid(packageName, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            fail(CaptureError.SourceNotFound(SourceId(PROJECTION, "app:$packageName")))
        }
    }
}

/**
 * Playback capture (`AudioPlaybackCaptureConfiguration`) into an audio
 * sink, on its own thread. Chunks of 10 ms are read straight into the
 * sink's Rust-owned slots (no copy).
 *
 * `wallUs` follows the sample count from an anchor (UNIX-epoch µs); the
 * anchor is set on the first chunk and again when the counted clock and
 * the `AudioTimestamp`-derived one drift apart by more than
 * [REANCHOR_US] (or after dropped chunks), and that chunk carries
 * `discontinuity = true`. Apps that opt out of capture, and apps
 * targeting API 28 or lower, are silent (`AudioCaps.mayBeSilent`).
 */
internal class PlaybackAudio(
    private val context: Context,
    private val plan: AudioPlan,
    private val projection: MediaProjection,
    private val sender: IstmoFrameSender<AudioMeta>,
    /** Called once, from the audio thread, when capture ends on its own (error, sink gone). */
    private val onEnd: (CaptureError?) -> Unit,
) {
    @Volatile
    var paused = false

    @Volatile
    var muted = false

    @Volatile
    private var running = false
    private var record: AudioRecord? = null
    private var thread: Thread? = null

    fun start() {
        val encoding = if (plan.config.format == SampleFormat.F32) AudioFormat.ENCODING_PCM_FLOAT else AudioFormat.ENCODING_PCM_16BIT
        val mask = if (plan.channels == 1) AudioFormat.CHANNEL_IN_MONO else AudioFormat.CHANNEL_IN_STEREO
        val format = AudioFormat.Builder()
            .setEncoding(encoding)
            .setSampleRate(plan.rate)
            .setChannelMask(mask)
            .build()
        val builder = AudioPlaybackCaptureConfiguration.Builder(projection)
        when (val source = plan.config.source) {
            is AudioSource.Application -> builder.addMatchingUid(AudioPlan.uidOf(context, source.value))
            AudioSource.ExcludeSelf -> {
                matchPlayback(builder)
                builder.excludeUid(Process.myUid())
            }
            AudioSource.System, is AudioSource.Node -> matchPlayback(builder)
        }
        val chunkBytes = plan.rate / CHUNKS_PER_SECOND * plan.bytesPerFrame
        val minimum = AudioRecord.getMinBufferSize(plan.rate, mask, encoding).coerceAtLeast(chunkBytes)
        val created = try {
            AudioRecord.Builder()
                .setAudioFormat(format)
                .setBufferSizeInBytes(minimum * 4)
                .setAudioPlaybackCaptureConfig(builder.build())
                .build()
        } catch (e: SecurityException) {
            fail(CaptureError.BackendUnavailable(BackendReason(PROJECTION, Availability.NeedsAccess(AccessScope.Microphone))))
        } catch (e: RuntimeException) {
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "AudioRecord", e.toString())))
        }
        if (created.state != AudioRecord.STATE_INITIALIZED) {
            created.release()
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "AudioRecord", "not initialised")))
        }
        if (!sender.openSlotPool(chunkBytes, SLOTS)) {
            created.release()
            fail(CaptureError.SinkUnreachable)
        }
        val started = try {
            created.startRecording()
            created.recordingState == AudioRecord.RECORDSTATE_RECORDING
        } catch (e: RuntimeException) {
            created.release()
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "AudioRecord.startRecording", e.toString())))
        }
        if (!started) {
            created.release()
            fail(CaptureError.Protocol(BackendDetail(PROJECTION, "AudioRecord.startRecording", "not recording")))
        }
        record = created
        running = true
        thread = Thread({ loop(created, chunkBytes) }, "istmo-capture-audio").apply {
            isDaemon = true
            start()
        }
    }

    private fun matchPlayback(builder: AudioPlaybackCaptureConfiguration.Builder) {
        builder.addMatchingUsage(AudioAttributes.USAGE_MEDIA)
        builder.addMatchingUsage(AudioAttributes.USAGE_GAME)
        builder.addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
    }

    private fun loop(record: AudioRecord, chunkBytes: Int) {
        val scratch = ByteBuffer.allocateDirect(chunkBytes)
        val stamp = AudioTimestamp()
        var framesRead = 0L
        var seq = 0L
        var anchorWallUs = Long.MIN_VALUE
        var anchorFrame = 0L
        var discontinuity = false
        var error: CaptureError? = null
        while (running) {
            val slot = sender.acquireSlot()
            val target = slot?.buffer ?: scratch.also { it.clear() }
            val read = record.read(target, chunkBytes, AudioRecord.READ_BLOCKING)
            if (read < 0) {
                slot?.let(sender::release)
                if (running) error = CaptureError.Protocol(BackendDetail(PROJECTION, "AudioRecord.read", "error $read"))
                break
            }
            val frames = read / plan.bytesPerFrame
            val startFrame = framesRead
            framesRead += frames
            if (slot == null || paused || frames == 0) {
                // The consumer holds every slot, or capture is paused: the
                // chunk is dropped and the next one says so.
                slot?.let(sender::release)
                if (frames > 0) discontinuity = true
                continue
            }
            val ptsNs = ptsOf(record, stamp, startFrame, frames)
            val measuredUs = CaptureClock.wallUsOf(ptsNs)
            val countedUs = anchorWallUs + (startFrame - anchorFrame) * 1_000_000L / plan.rate
            if (anchorWallUs == Long.MIN_VALUE || discontinuity || abs(countedUs - measuredUs) > REANCHOR_US) {
                if (anchorWallUs != Long.MIN_VALUE) discontinuity = true
                anchorWallUs = measuredUs
                anchorFrame = startFrame
            }
            val wallUs = anchorWallUs + (startFrame - anchorFrame) * 1_000_000L / plan.rate
            if (muted) {
                val zeros = slot.buffer.duplicate()
                for (i in 0 until read) zeros.put(i, 0.toByte())
            }
            val meta = AudioMeta(
                seq = seq.toULong(),
                ptsNs = ptsNs.toULong(),
                wallUs = wallUs.toULong(),
                rate = plan.rate.toUInt(),
                channels = plan.channels.toUByte(),
                format = plan.config.format,
                frames = frames.toUInt(),
                discontinuity = discontinuity,
            )
            seq += 1
            discontinuity = false
            val status = sender.submit(slot, read, meta)
            if (status.shouldStop) {
                if (running) error = CaptureError.SinkUnreachable
                break
            }
        }
        val ownEnd = running
        running = false
        runCatching { record.stop() }
        record.release()
        if (ownEnd) {
            Log.w(TAG, "playback capture ended: $error")
            onEnd(error)
        }
    }

    /** Capture time of the chunk's first frame on `CLOCK_MONOTONIC`. */
    private fun ptsOf(record: AudioRecord, stamp: AudioTimestamp, startFrame: Long, frames: Int): Long {
        if (record.getTimestamp(stamp, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
            return stamp.nanoTime + (startFrame - stamp.framePosition) * 1_000_000_000L / plan.rate
        }
        // No timestamp yet: the chunk just finished arriving.
        return System.nanoTime() - frames * 1_000_000_000L / plan.rate
    }

    /**
     * Stop reading without waiting: the reader thread sees [running] after
     * its current chunk (10 ms) and stops and releases the record itself,
     * so the caller's thread (the capture thread, delivering frames) is
     * never held up. The record is never touched from here.
     */
    fun stop() {
        running = false
    }

    private companion object {
        const val TAG = "istmo.screen_capture"
        const val CHUNKS_PER_SECOND = 100
        const val SLOTS = 32
        const val REANCHOR_US = 20_000L
    }
}
