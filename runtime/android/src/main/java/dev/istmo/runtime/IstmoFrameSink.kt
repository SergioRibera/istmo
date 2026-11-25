package dev.istmo.runtime

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer

/**
 * What a frame sink did with a submitted frame; mirrors Rust's
 * `istmo_core::native_frame::SubmitStatus`.
 *
 * Unless the status [isTaken], Rust kept nothing: the caller still owns
 * its buffer, as it always does (Rust takes references of its own).
 */
enum class IstmoSubmitStatus(val code: Int) {
    /** Queued. */
    ACCEPTED(0),
    /** Queued after evicting an older frame. */
    DISPLACED(1),
    /** The consumer is gone or the sink was closed: stop producing. */
    CLOSED(2),
    /** No such sink in this process (stale, or opened elsewhere). */
    UNKNOWN_SINK(3),
    /** The metadata does not decode as the sink's metadata type. */
    BAD_META(4),
    /** The platform cannot do this (a `HardwareBuffer` below API 26). */
    UNSUPPORTED(5),
    /** The Rust runtime is not running. */
    NOT_STARTED(6),
    /** A malformed argument. */
    INVALID(7),
    /** The sink is full and refuses new frames. */
    FULL(8),
    ;

    /** Whether the sink kept the frame. */
    val isTaken: Boolean
        get() = this == ACCEPTED || this == DISPLACED

    companion object {
        fun fromCode(code: Int): IstmoSubmitStatus = entries.firstOrNull { it.code == code } ?: INVALID
    }
}

/**
 * A Rust frame sink a backend feeds: frames go straight to the Rust
 * consumer, metadata wire-encoded, buffers never copied (except
 * [submitBytes]).
 *
 * Generated dispatchers build it from the `FrameSinkId<M>` argument of a
 * plugin method; backends never construct one.
 */
class IstmoFrameSink<M>(
    /** The sink's id, as Rust allocated it. */
    val id: Long,
    private val writeMeta: (ByteArrayOutputStream, M) -> Unit,
) {
    private fun encode(meta: M): ByteArray {
        val out = ByteArrayOutputStream()
        writeMeta(out, meta)
        return out.toByteArray()
    }

    /**
     * Hand a `HardwareBuffer` (API 26+) to Rust, which takes its own
     * reference: close the buffer, or the `Image` it came from, whenever
     * convenient.
     */
    fun submit(buffer: android.hardware.HardwareBuffer, meta: M): IstmoSubmitStatus =
        IstmoSubmitStatus.fromCode(IstmoRuntime.nativeSubmitHardwareBuffer(id, buffer, encode(meta)))

    /**
     * Hand the first [length] bytes of a direct `ByteBuffer` to Rust,
     * read in place: do not write the buffer afterwards.
     */
    fun submit(buffer: ByteBuffer, length: Int, meta: M): IstmoSubmitStatus {
        require(buffer.isDirect) { "IstmoFrameSink.submit needs a direct ByteBuffer; use submitBytes" }
        return IstmoSubmitStatus.fromCode(IstmoRuntime.nativeSubmitDirectBuffer(id, buffer, length, encode(meta)))
    }

    /** Copy [bytes] into a frame (one copy; for heap arrays). */
    fun submitBytes(bytes: ByteArray, meta: M): IstmoSubmitStatus =
        IstmoSubmitStatus.fromCode(IstmoRuntime.nativeSubmitFrameBytes(id, bytes, encode(meta)))

    /** End the stream: the consumer receives what is queued, then the end. */
    fun close(): Boolean = IstmoRuntime.nativeCloseFrameSink(id)
}
