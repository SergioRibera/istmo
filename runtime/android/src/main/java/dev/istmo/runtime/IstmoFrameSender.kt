package dev.istmo.runtime

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.util.concurrent.ConcurrentHashMap

/**
 * What a frame sink did with a submitted frame; mirrors Rust's
 * `istmo_core::native_frame::SubmitStatus` (the codes are checked
 * against it by a test).
 */
enum class IstmoSubmitStatus(val code: Int) {
    /** Queued. */
    ACCEPTED(0),
    /** Queued after evicting an older frame. */
    DISPLACED(1),
    /** The sink ended (consumer gone, closed, shutdown): stop producing. */
    CLOSED(2),
    /** No such sink in this process (stale, or opened elsewhere). */
    UNKNOWN_SINK(3),
    /** The metadata does not decode as the sink's metadata type. */
    BAD_META(4),
    /** The platform cannot do this (a `HardwareBuffer` below API 26). */
    UNSUPPORTED(5),
    /** The Rust runtime is not running. */
    NOT_STARTED(6),
    /** A malformed argument, or a sender already closed. */
    INVALID(7),
    /** The sink is full and refuses new frames. */
    FULL(8),
    /** Rust failed internally after taking the buffer (a bug); Rust releases it. */
    INTERNAL(9),
    /** The sink carries another metadata type than this sender's. */
    TYPE_MISMATCH(10),
    ;

    /** Whether the sink kept the frame. */
    val isTaken: Boolean
        get() = this == ACCEPTED || this == DISPLACED

    /** Whether every later submit fails the same way: stop producing. */
    val shouldStop: Boolean
        get() = this == CLOSED || this == UNKNOWN_SINK || this == NOT_STARTED || this == TYPE_MISMATCH

    companion object {
        fun fromCode(code: Int): IstmoSubmitStatus = entries.firstOrNull { it.code == code } ?: INTERNAL
    }
}

/**
 * Marks the constructor generated dispatchers use: app and backend code
 * receives senders as plugin arguments and never builds one.
 */
@RequiresOptIn(
    message = "IstmoFrameSender is built by generated dispatchers only",
    level = RequiresOptIn.Level.ERROR,
)
@Retention(AnnotationRetention.BINARY)
@Target(AnnotationTarget.FUNCTION)
annotation class IstmoGeneratedApi

/** One Rust-owned pool slot a sender lent out: fill [buffer], then submit it. */
class IstmoFrameSlot internal constructor(
    /** The slot's index in the pool. */
    val index: Int,
    /** The slot's memory; valid until the slot is submitted or given back. */
    val buffer: ByteBuffer,
)

/**
 * The producer end of a Rust frame sink, handed to a backend as the
 * `FrameSinkId<M>` argument of a plugin method. One sender is one
 * producer session: the sink ends when it closes ([close],
 * [closeWithError]) or, if never closed, once it is garbage collected.
 * Close it when the session ends.
 *
 * Buffers:
 * - [submit] (`Image`, `HardwareBuffer` + owner): zero copy. Rust takes
 *   its own `AHardwareBuffer` reference and the sender keeps the owner
 *   (the `Image`) open until Rust is done with the buffer, then closes
 *   it on the sender's releaser thread. Size an `ImageReader`'s
 *   `maxImages` for the frames the consumer may hold plus one.
 * - [openSlotPool] / [acquireSlot] / [submit] (slot): zero copy into
 *   Rust-owned memory, for CPU data (audio, encoded packets).
 * - [submit] (`ByteBuffer`) and [submitBytes]: one copy; the caller keeps
 *   its buffer.
 *
 * Thread-safe; submits are serialised.
 */
class IstmoFrameSender<M> private constructor(
    /** The sink's id, as Rust allocated it (an unsigned 64-bit value). */
    val sinkId: Long,
    fingerprint: Long,
    private val writeMeta: (ByteArrayOutputStream, M) -> Unit,
) : AutoCloseable {
    private class MetaBuffer : ByteArrayOutputStream(64) {
        fun bytes(): ByteArray = buf
    }

    private val lock = Any()
    private val meta = MetaBuffer()
    private var handle: Long
    private val openStatus: IstmoSubmitStatus
    private var ended = false
    private val owners = ConcurrentHashMap<Long, AutoCloseable>()
    private var nextTicket = 1L
    private var releaser: Thread? = null
    private var slots: Array<ByteBuffer>? = null

    init {
        val status = IntArray(1)
        handle = IstmoFrames.nativeOpenProducer(sinkId, fingerprint, status)
        openStatus = if (handle == 0L) IstmoSubmitStatus.fromCode(status[0]) else IstmoSubmitStatus.ACCEPTED
    }

    /** Why the sender cannot submit at all, if it cannot. */
    val failure: IstmoSubmitStatus?
        get() = if (handle == 0L && !ended) openStatus else null

    /** Whether [close] ran or the sender never reached its sink. */
    val isClosed: Boolean
        get() = synchronized(lock) { ended || handle == 0L }

    private inline fun submitLocked(value: M, call: (Long, ByteArray, Int) -> Int): IstmoSubmitStatus =
        synchronized(lock) {
            if (ended) return IstmoSubmitStatus.CLOSED
            if (handle == 0L) return openStatus
            meta.reset()
            writeMeta(meta, value)
            IstmoSubmitStatus.fromCode(call(handle, meta.bytes(), meta.size()))
        }

    /**
     * Hand a `HardwareBuffer` to Rust (API 26+). Rust takes its own
     * reference: close the buffer whenever convenient. When the buffer
     * belongs to a pool its producer reuses (an `Image` of an
     * `ImageReader`), pass that as [owner]: the sender closes it once Rust
     * no longer reads the buffer, whatever the status.
     */
    fun submit(buffer: android.hardware.HardwareBuffer, meta: M, owner: AutoCloseable? = null): IstmoSubmitStatus {
        if (android.os.Build.VERSION.SDK_INT < 26) {
            owner?.close()
            return IstmoSubmitStatus.UNSUPPORTED
        }
        if (buffer.isClosed) {
            owner?.close()
            return IstmoSubmitStatus.INVALID
        }
        val ticket = if (owner == null) 0L else lend(owner)
        var reached = false
        val status = submitLocked(meta) { h, bytes, len ->
            reached = true
            IstmoFrames.nativeSubmitHardwareBuffer(h, buffer, ticket, bytes, len)
        }
        // Rust reports every ticket it was handed, refused or not; one it
        // never saw is ours to close.
        if (ticket != 0L && !reached) owners.remove(ticket)?.close()
        return status
    }

    /**
     * Hand an `Image`'s buffer to Rust (API 28+), keeping the image open
     * until Rust is done with it. Waits (up to 1 s) for the image's
     * acquire fence on API 33+, so Rust only sees finished pixels.
     */
    fun submit(image: android.media.Image, meta: M): IstmoSubmitStatus {
        if (android.os.Build.VERSION.SDK_INT < 28) {
            image.close()
            return IstmoSubmitStatus.UNSUPPORTED
        }
        if (android.os.Build.VERSION.SDK_INT >= 33) {
            val fence = image.fence
            if (fence.isValid) fence.await(java.time.Duration.ofSeconds(1))
        }
        val buffer = image.hardwareBuffer
        if (buffer == null) {
            image.close()
            return IstmoSubmitStatus.UNSUPPORTED
        }
        return try {
            submit(buffer, meta, owner = image)
        } finally {
            buffer.close()
        }
    }

    /** Copy the remaining bytes of [buffer] (direct or heap) into a frame. */
    fun submit(buffer: ByteBuffer, meta: M): IstmoSubmitStatus {
        val offset = buffer.position()
        val length = buffer.remaining()
        return if (buffer.isDirect) {
            submitLocked(meta) { h, bytes, len ->
                IstmoFrames.nativeSubmitDirectBuffer(h, buffer, offset, length, bytes, len)
            }
        } else {
            submitBytes(buffer.array(), meta, buffer.arrayOffset() + offset, length)
        }
    }

    /** Copy [length] bytes of [bytes] from [offset] into a frame. */
    fun submitBytes(bytes: ByteArray, meta: M, offset: Int = 0, length: Int = bytes.size - offset): IstmoSubmitStatus =
        submitLocked(meta) { h, m, len -> IstmoFrames.nativeSubmitBytes(h, bytes, offset, length, m, len) }

    /**
     * Create this sender's pool: [count] (1..64) Rust-owned slots of
     * [slotSize] bytes. Once per sender; `false` when the shape is invalid
     * or the sender is closed.
     */
    fun openSlotPool(slotSize: Int, count: Int): Boolean = synchronized(lock) {
        if (ended || handle == 0L) return false
        if (slots == null) {
            @Suppress("UNCHECKED_CAST")
            slots = IstmoFrames.nativeOpenSlotPool(handle, slotSize, count) as Array<ByteBuffer>?
        }
        slots != null
    }

    /** A free slot to fill, or `null` while the consumer holds them all. */
    fun acquireSlot(): IstmoFrameSlot? = synchronized(lock) {
        val pool = slots ?: return null
        if (ended) return null
        val index = IstmoFrames.nativeAcquireSlot(handle)
        if (index < 0) return null
        val view = pool[index].duplicate()
        view.clear()
        IstmoFrameSlot(index, view)
    }

    /** Submit the first [length] bytes of [slot], no copy. The slot is Rust's afterwards. */
    fun submit(slot: IstmoFrameSlot, length: Int, meta: M): IstmoSubmitStatus =
        submitLocked(meta) { h, bytes, len -> IstmoFrames.nativeSubmitSlot(h, slot.index, length, bytes, len) }

    /** Give an acquired slot back without submitting it. */
    fun release(slot: IstmoFrameSlot) {
        synchronized(lock) {
            if (!ended && handle != 0L) IstmoFrames.nativeReleaseSlot(handle, slot.index)
        }
    }

    /** End the stream: the consumer receives what is queued, then the end. */
    override fun close() = end(null)

    /** End the stream with an error the consumer reads as the end reason. */
    fun closeWithError(message: String) = end(message)

    private fun end(error: String?) {
        synchronized(lock) {
            if (ended || handle == 0L) {
                ended = true
                return
            }
            ended = true
            IstmoFrames.nativeEndProducer(handle, error)
            slots = null
            if (releaser == null) freeLocked()
        }
    }

    private fun freeLocked() {
        if (handle != 0L) {
            IstmoFrames.nativeFreeProducer(handle)
            handle = 0L
        }
    }

    /** Register [owner] under a fresh ticket and make sure a releaser runs. */
    private fun lend(owner: AutoCloseable): Long = synchronized(lock) {
        val ticket = nextTicket++
        owners[ticket] = owner
        if (releaser == null && handle != 0L && !ended) {
            releaser = Thread({ releaseLoop() }, "istmo-frame-releaser").apply {
                isDaemon = true
                start()
            }
        }
        ticket
    }

    /** Close owners as Rust releases their buffers; free the producer at the end. */
    private fun releaseLoop() {
        while (true) {
            val h = synchronized(lock) {
                if (ended && owners.isEmpty()) {
                    freeLocked()
                    releaser = null
                    return
                }
                handle
            }
            val tickets = IstmoFrames.nativeAwaitReleased(h, 250) ?: continue
            for (ticket in tickets) {
                runCatching { owners.remove(ticket)?.close() }
            }
        }
    }

    @Suppress("ProtectedInFinal", "unused")
    protected fun finalize() {
        end(null)
    }

    companion object {
        /**
         * Open a sender on the sink [sinkId] whose metadata has the type
         * [fingerprint] names; only generated dispatchers call this.
         */
        @IstmoGeneratedApi
        fun <M> open(sinkId: Long, fingerprint: Long, writeMeta: (ByteArrayOutputStream, M) -> Unit): IstmoFrameSender<M> =
            IstmoFrameSender(sinkId, fingerprint, writeMeta)
    }
}

/**
 * The frame-sink entries of the Rust library (`istmo_android`'s
 * `frame_exports`), named `Java_dev_istmo_runtime_IstmoFrames_*`.
 */
internal object IstmoFrames {
    external fun nativeAbiVersion(): Int
    external fun nativeOpenProducer(sink: Long, fingerprint: Long, status: IntArray): Long
    external fun nativeEndProducer(handle: Long, error: String?)
    external fun nativeFreeProducer(handle: Long)
    external fun nativeSubmitHardwareBuffer(
        handle: Long,
        buffer: android.hardware.HardwareBuffer,
        ticket: Long,
        meta: ByteArray,
        metaLen: Int,
    ): Int
    external fun nativeSubmitBytes(handle: Long, bytes: ByteArray, offset: Int, length: Int, meta: ByteArray, metaLen: Int): Int
    external fun nativeSubmitDirectBuffer(
        handle: Long,
        buffer: ByteBuffer,
        offset: Int,
        length: Int,
        meta: ByteArray,
        metaLen: Int,
    ): Int
    external fun nativeOpenSlotPool(handle: Long, slotSize: Int, count: Int): Array<Any?>?
    external fun nativeAcquireSlot(handle: Long): Int
    external fun nativeReleaseSlot(handle: Long, slot: Int): Boolean
    external fun nativeSubmitSlot(handle: Long, slot: Int, length: Int, meta: ByteArray, metaLen: Int): Int
    external fun nativeAwaitReleased(handle: Long, timeoutMs: Long): LongArray?

    /** The native entry version this runtime was built against. */
    const val ABI_VERSION = 1

    /** Throw when the loaded library speaks another entry version. */
    fun checkAbi() {
        val found = try {
            nativeAbiVersion()
        } catch (missing: UnsatisfiedLinkError) {
            throw IllegalStateException(
                "the istmo native library predates this runtime (no nativeAbiVersion); rebuild it",
                missing,
            )
        }
        check(found == ABI_VERSION) {
            "istmo native library ABI $found, runtime expects $ABI_VERSION: rebuild the app's Rust library " +
                "with the matching istmo version"
        }
    }
}
