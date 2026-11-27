package dev.istmo.plugins.screencapture

import android.hardware.HardwareBuffer
import android.media.Image
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.view.Surface
import dev.istmo.runtime.IstmoFrameSender
import dev.istmo.runtime.IstmoSubmitStatus
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.atomic.AtomicLong
import kotlinx.coroutines.channels.Channel
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow

/**
 * Events of one session: every subscriber first gets [started], then the
 * newest [HISTORY] events, then live ones until the session ends.
 */
internal class EventHub<E : Any>(@Volatile var started: E) {
    private val lock = Any()
    private val history = ArrayDeque<E>()
    private val subscribers = ArrayList<Channel<E>>()
    private var closed = false

    fun publish(event: E) {
        synchronized(lock) {
            if (closed) return
            history.addLast(event)
            while (history.size > HISTORY) history.removeFirst()
            for (channel in subscribers) channel.trySend(event)
        }
    }

    /** No more events: live subscribers' flows complete after what they hold. */
    fun close() {
        synchronized(lock) {
            closed = true
            for (channel in subscribers) channel.close()
            subscribers.clear()
        }
    }

    fun flow(): Flow<E> = flow {
        val channel = Channel<E>(Channel.UNLIMITED)
        synchronized(lock) {
            channel.trySend(started)
            for (event in history) channel.trySend(event)
            if (closed) channel.close() else subscribers.add(channel)
        }
        try {
            for (event in channel) emit(event)
        } finally {
            synchronized(lock) { subscribers.remove(channel) }
        }
    }

    private companion object {
        const val HISTORY = 64
    }
}

/** What a frame says about where it came from. */
internal class FrameOrigin(val source: SourceId, val scalePermille: UInt)

/**
 * `ImageReader` images into a frame sink, on the backend's capture
 * thread ([handler]).
 *
 * - [Offer.Gpu]: each image's `HardwareBuffer` is submitted with the
 *   image as its owner, so the image stays open until Rust drops the
 *   frame. A buffer is delivered only while the frames already out leave
 *   one image for the producer ("never hand out the last free buffer");
 *   otherwise the new image is closed at once and counted in the next
 *   frame's `skippedBefore` (`Stall` and `DropNewest` behave the same: a
 *   virtual display cannot be paused per buffer).
 * - [Offer.Cpu]: the image's rows are copied into the sink and the image
 *   closed (`CopyStage.GpuReadback`: the GPU composed the image).
 *
 * Pacing follows `FrameRate`: `Max` / `OnDamage` defer a frame inside the
 * period and deliver the newest at its end (a virtual display only
 * produces frames on change, so both behave alike); `Exact` repeats the
 * last frame (same buffer, `repeat = true`) while the screen is idle.
 */
internal class FrameStream(
    private val handler: Handler,
    private val sender: IstmoFrameSender<FrameMeta>,
    offer: Offer,
    private val origin: FrameOrigin,
    /** Images per reader, the frames the pacer keeps included. */
    poolDepth: Int,
    private val exhaustion: Exhaustion,
    fps: FrameRate,
    private val events: (CaptureEvent) -> Unit,
    private val onSinkGone: () -> Unit,
) {
    @Volatile
    var fps: FrameRate = fps
        private set

    @Volatile
    var offer: Offer = offer
        private set

    /** Images per reader. */
    @Volatile
    var poolDepth: Int = poolDepth
        private set

    /** Index of the caller's format tier the offer met. */
    @Volatile
    var tier: Int = 0
        private set

    /** Use [offer] (tier [tier]) and [pool] images from the next [open]. Capture thread. */
    fun configure(offer: Offer, tier: Int, pool: Int) {
        this.offer = offer
        this.tier = tier
        poolDepth = pool
    }

    /** Valid area of each frame, in buffer pixels. */
    @Volatile
    var contentRect: Rect = Rect(0, 0, 0u, 0u)

    @Volatile
    var paused = false

    /** Called (capture thread) after each delivered frame. */
    @Volatile
    var onDelivered: ((FrameMeta) -> Unit)? = null

    private var reader: Reader? = null
    private var ended = false
    private var seq = 0L
    private var skipped = 0
    private var exhaustedEpisode = false
    private var episodes = 0
    private var lastDeliveryNs = Long.MIN_VALUE / 2
    private var pending: Lease? = null
    private var last: Lease? = null
    private var flushScheduled = false
    private val ticker = Runnable { tick() }
    private val flusher = Runnable {
        flushScheduled = false
        flush()
    }

    /** The current reader's size. */
    val size: Size?
        get() = reader?.let { Size(it.width.toUInt(), it.height.toUInt()) }

    /** One image of a reader, open until both the stream and every frame sharing it let go. */
    private inner class Lease(val image: Image, val owner: Reader) : AutoCloseable {
        private val refs = AtomicInteger(1)

        fun retain() {
            refs.incrementAndGet()
        }

        /** A sink frame's release (the sender closes owners on its releaser thread). */
        override fun close() {
            if (refs.decrementAndGet() == 0) {
                runCatching { image.close() }
                owner.returned()
            }
        }
    }

    private inner class Reader(val width: Int, val height: Int) : ImageReader.OnImageAvailableListener {
        val generation: Long = GENERATIONS.incrementAndGet()
        val reader: ImageReader = newReader(width, height)
        val open = AtomicInteger(0)
        private val slots = HashMap<Long, Int>()
        private var retired = false
        private var closed = false

        init {
            reader.setOnImageAvailableListener(this, handler)
        }

        val surface: Surface get() = reader.surface

        override fun onImageAvailable(reader: ImageReader) {
            while (!retired) {
                val image = try {
                    reader.acquireNextImage()
                } catch (e: IllegalStateException) {
                    // Every image is out: the frames held count as an
                    // exhaustion episode until one comes back.
                    exhausted()
                    return
                } ?: return
                open.incrementAndGet()
                arrive(Lease(image, this))
            }
        }

        fun slot(buffer: HardwareBuffer): UInt {
            if (Build.VERSION.SDK_INT < Api.S) return UNKNOWN_SLOT
            return slots.getOrPut(buffer.id) { slots.size }.toUInt()
        }

        /** No new images; the reader closes once its last image is back. */
        fun retire() {
            retired = true
            reader.setOnImageAvailableListener(null, null)
            closeIfDrained()
        }

        fun returned() {
            if (open.decrementAndGet() == 0) handler.post { closeIfDrained() }
        }

        private fun closeIfDrained() {
            if (retired && !closed && open.get() == 0) {
                closed = true
                reader.close()
            }
        }
    }

    private fun newReader(width: Int, height: Int): ImageReader = when (offer) {
        Offer.Gpu ->
            try {
                ImageReader.newInstance(
                    width, height, android.graphics.PixelFormat.RGBA_8888, poolDepth,
                    HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE,
                )
            } catch (e: IllegalArgumentException) {
                // RGBA_8888 with GPU usage is outside the documented
                // combinations; PRIVATE is the documented one.
                ImageReader.newInstance(
                    width, height, android.graphics.ImageFormat.PRIVATE, poolDepth,
                    HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE,
                )
            }
        Offer.Cpu -> ImageReader.newInstance(width, height, android.graphics.PixelFormat.RGBA_8888, poolDepth)
    }

    /** Start (or restart, after a resize or a format change) with a fresh pool. Capture thread. */
    fun open(width: Int, height: Int): Surface {
        reader?.retire()
        dropHeld()
        val next = Reader(width, height)
        reader = next
        if (contentRect.width == 0u) contentRect = Rect(0, 0, width.toUInt(), height.toUInt())
        handler.removeCallbacks(ticker)
        schedule()
        return next.surface
    }

    /** The current pool's identity (bumps on every `open`). */
    val generation: Long get() = reader?.generation ?: 0L

    /** Capture thread; reopen with [poolFor] the new rate when it keeps more images. */
    fun updateFps(rate: FrameRate) {
        fps = rate
        handler.removeCallbacks(ticker)
        schedule()
    }

    /** Capture thread. */
    fun close() {
        if (ended) return
        ended = true
        handler.removeCallbacks(ticker)
        handler.removeCallbacks(flusher)
        dropHeld()
        reader?.retire()
        reader = null
    }

    private fun dropHeld() {
        pending?.close()
        pending = null
        last?.close()
        last = null
    }

    private fun schedule() {
        val rate = fps
        if (rate is FrameRate.Exact && !ended) handler.postDelayed(ticker, periodMs(rate.value))
    }

    private fun periodNs(perSecond: UInt): Long = 1_000_000_000L / perSecond.toLong().coerceAtLeast(1)

    private fun periodMs(perSecond: UInt): Long = (periodNs(perSecond) / 1_000_000L).coerceAtLeast(1)

    private fun arrive(lease: Lease) {
        if (ended || lease.owner !== reader || paused) {
            if (paused) skipped += 1
            lease.close()
            return
        }
        when (val rate = fps) {
            FrameRate.Native -> deliverFresh(lease)
            is FrameRate.Max -> pace(lease, rate.value)
            is FrameRate.OnDamage -> pace(lease, rate.value)
            is FrameRate.Exact -> {
                pending?.let {
                    skipped += 1
                    it.close()
                }
                pending = lease
            }
        }
    }

    private fun pace(lease: Lease, perSecond: UInt) {
        val period = periodNs(perSecond)
        val now = System.nanoTime()
        if (pending == null && now - lastDeliveryNs >= period - period / 20) {
            deliverFresh(lease)
            return
        }
        pending?.let {
            skipped += 1
            it.close()
        }
        pending = lease
        if (!flushScheduled) {
            flushScheduled = true
            val waitNs = (lastDeliveryNs + period - now).coerceAtLeast(0)
            handler.postDelayed(flusher, waitNs / 1_000_000L)
        }
    }

    private fun flush() {
        val next = pending ?: return
        pending = null
        deliverFresh(next)
    }

    private fun tick() {
        if (ended) return
        val fresh = pending
        pending = null
        if (fresh != null) {
            deliverFresh(fresh)
        } else {
            last?.let { deliver(it, repeat = true) }
        }
        schedule()
    }

    /** Deliver a new image, then keep it for repeats (`Exact`) or let go of it. */
    private fun deliverFresh(lease: Lease) {
        val delivered = deliver(lease, repeat = false)
        if (delivered && fps is FrameRate.Exact) {
            last?.close()
            last = lease
        } else {
            lease.close()
        }
    }

    private fun exhausted() {
        skipped += 1
        if (exhaustedEpisode) return
        exhaustedEpisode = true
        episodes += 1
        events(CaptureEvent.Exhausted(ExhaustionReport(episodes.toUInt(), poolDepth.toUByte(), exhaustion)))
    }

    /** Whether the sink took the frame. */
    private fun deliver(lease: Lease, repeat: Boolean): Boolean {
        val image = lease.image
        // Every open image (this one, the pacer's, the consumer's) counts:
        // delivering the image that left the producer none would let a
        // consumer holding frames starve it.
        if (offer == Offer.Gpu && !repeat && lease.owner.open.get() >= poolDepth) {
            exhausted()
            return false
        }
        val ptsNs = if (repeat) System.nanoTime() else image.timestamp
        val status = when (offer) {
            Offer.Gpu -> submitBuffer(lease, ptsNs, repeat)
            Offer.Cpu -> submitCopy(image, lease.owner, ptsNs, repeat)
        } ?: return false
        when {
            status.isTaken -> {
                exhaustedEpisode = false
                skipped = 0
                lastDeliveryNs = System.nanoTime()
            }
            status.shouldStop -> onSinkGone()
            else -> {}
        }
        return status.isTaken
    }

    private fun meta(
        owner: Reader,
        format: PixelFormat,
        planes: List<PlaneLayout>,
        slot: UInt,
        ptsNs: Long,
        repeat: Boolean,
    ): FrameMeta {
        val value = FrameMeta(
            seq = seq.toULong(),
            ptsNs = ptsNs.toULong(),
            ptsClock = PtsClock.Monotonic,
            wallUs = CaptureClock.wallUsOf(ptsNs).toULong(),
            size = Size(owner.width.toUInt(), owner.height.toUInt()),
            contentRect = contentRect,
            format = format,
            memory = offer.memory,
            planes = planes,
            modifier = null,
            buffer = BufferIdentity(owner.generation.toULong(), slot),
            tier = tier.toUByte(),
            copied = null,
            producer = null,
            colour = SRGB,
            hdr = null,
            orientation = UPRIGHT,
            damage = Damage.Full,
            cursor = null,
            sync = SyncKind.Ready,
            scalePermille = origin.scalePermille,
            sourceId = origin.source,
            repeat = repeat,
            skippedBefore = skipped.toUInt(),
            protectedContent = false,
            composite = null,
        )
        seq += 1
        return value
    }

    private fun submitBuffer(lease: Lease, ptsNs: Long, repeat: Boolean): IstmoSubmitStatus? {
        val image = lease.image
        if (!repeat && Build.VERSION.SDK_INT >= Api.TIRAMISU) {
            // Rust only sees finished pixels. Each `fence` is a fresh fd.
            image.fence.use { fence -> if (fence.isValid) fence.await(FENCE_TIMEOUT) }
        }
        val buffer = image.hardwareBuffer ?: return null
        val format = when (buffer.format) {
            HardwareBuffer.RGBA_8888 -> PixelFormat.Rgba8
            HardwareBuffer.RGBX_8888 -> PixelFormat.Rgbx8
            // PRIVATE allocations: the display composes RGBA.
            else -> offer.format
        }
        val meta = meta(lease.owner, format, emptyList(), lease.owner.slot(buffer), ptsNs, repeat)
        lease.retain()
        return try {
            sender.submit(buffer, meta, lease)
        } finally {
            buffer.close()
        }.also { status -> if (status.isTaken) onDelivered?.invoke(meta) }
    }

    private fun submitCopy(image: Image, owner: Reader, ptsNs: Long, repeat: Boolean): IstmoSubmitStatus {
        val plane = image.planes[0]
        val rowStride = plane.rowStride
        val view = plane.buffer.duplicate()
        view.rewind()
        val length = minOf(view.remaining(), rowStride * (owner.height - 1) + owner.width * plane.pixelStride)
        view.limit(length)
        val meta = meta(
            owner,
            offer.format,
            listOf(PlaneLayout(0u, rowStride.toUInt(), length.toULong())),
            UNKNOWN_SLOT,
            ptsNs,
            repeat,
        )
        return sender.submit(view, meta).also { status -> if (status.isTaken) onDelivered?.invoke(meta) }
    }

    companion object {
        /** No reusable buffer behind the frame (a copy), or the OS does not say which (API < 31). */
        val UNKNOWN_SLOT: UInt = UInt.MAX_VALUE

        private val GENERATIONS = AtomicLong(0)

        private val FENCE_TIMEOUT by lazy { java.time.Duration.ofSeconds(1) }

        /** Pool depth when the caller leaves it to the backend. */
        const val DEFAULT_POOL = 4
        const val MIN_POOL = 2
        const val MAX_POOL = 16

        /** The images a reader needs: the caller's depth plus what pacing at [fps] keeps. */
        fun poolFor(requested: UByte, fps: FrameRate): Int {
            val base = if (requested == 0.toUByte()) DEFAULT_POOL else requested.toInt()
            val kept = when (fps) {
                FrameRate.Native -> 0
                is FrameRate.Max, is FrameRate.OnDamage -> 1
                is FrameRate.Exact -> 2
            }
            return (base + kept).coerceIn(MIN_POOL, MAX_POOL)
        }
    }
}
