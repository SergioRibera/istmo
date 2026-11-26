package dev.istmo.bench

import android.graphics.Color
import android.graphics.PixelFormat
import android.hardware.HardwareBuffer
import android.media.ImageReader
import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import android.os.SystemClock
import dev.istmo.bench.gen.FrameMeta
import dev.istmo.bench.gen.FrameReport
import dev.istmo.bench.gen.KotlinBenchBackend
import dev.istmo.runtime.IstmoFrameSender
import dev.istmo.runtime.IstmoSubmitStatus
import java.util.concurrent.ArrayBlockingQueue
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.locks.LockSupport
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.flow
import kotlinx.coroutines.withContext

/** How a frame cell hands buffers to Rust; mirrors `cells::FramePath`. */
enum class FramePath(val id: Int) { Bytes(0), Slot(1), HardwareBuffer(2), Image(3) }

/** The Kotlin end of the `KotlinBench` plugin that Rust calls. */
class BenchBackend : KotlinBenchBackend {
    override suspend fun echo(payload: ByteArray): ByteArray {
        Probe.stamp(Probe.BACKEND_ENTER)
        Probe.stamp(Probe.BACKEND_EXIT)
        return payload
    }

    // Emitted on the thread that collects (the runtime collects with
    // `Dispatchers.Unconfined` on the call's worker): no extra hop, like
    // a sensor callback emitting straight into the flow.
    override fun ticks(count: UInt, size: UInt, gap_us: UInt): Flow<ByteArray> = flow {
        val bytes = maxOf(8, size.toInt())
        val gapNs = gap_us.toLong() * 1_000
        repeat(count.toInt()) {
            if (gapNs > 0) LockSupport.parkNanos(gapNs)
            val item = ByteArray(bytes)
            putLongLe(item, System.nanoTime())
            // The last item says which CPU produced it (placement decides
            // this cell on big.LITTLE parts).
            if (bytes >= 12 && it == count.toInt() - 1) putIntLe(item, 8, BenchNative.currentCpu())
            emit(item)
        }
    }

    override suspend fun frames(
        path: UInt,
        count: UInt,
        size: UInt,
        gap_us: UInt,
        frames: IstmoFrameSender<FrameMeta>,
    ): FrameReport = withContext(Dispatchers.IO) {
        val producer = FrameProducer(count.toInt(), size.toInt(), gap_us.toLong() * 1_000, frames)
        try {
            when (path.toInt()) {
                FramePath.Bytes.id -> producer.bytes()
                FramePath.Slot.id -> producer.slots()
                FramePath.HardwareBuffer.id -> producer.hardwareBuffers()
                else -> producer.images()
            }
        } catch (e: Throwable) {
            producer.note = "producer threw ${e.javaClass.simpleName}: ${e.message}"
        } finally {
            frames.close()
        }
        producer.report()
    }
}

private fun putLongLe(dst: ByteArray, value: Long) {
    for (i in 0 until 8) dst[i] = (value ushr (8 * i)).toByte()
}

private fun putIntLe(dst: ByteArray, at: Int, value: Int) {
    for (i in 0 until 4) dst[at + i] = (value ushr (8 * i)).toByte()
}

/** Width × height of an RGBA buffer of [size] bytes (1080p for the 8 MB cell). */
private fun rgbaShape(size: Int): Pair<Int, Int> = when (size) {
    1920 * 1080 * 4 -> 1920 to 1080
    else -> {
        val side = maxOf(1, Math.sqrt(size / 4.0).toInt())
        side to side
    }
}

private class FrameProducer(
    private val count: Int,
    private val size: Int,
    private val gapNs: Long,
    private val sender: IstmoFrameSender<FrameMeta>,
) {
    val cost = LongArray(count)
    val released = LongArray(count)
    var notTaken = 0
    var firstRefusal = -1
    var pendingFences = 0
    var note = ""

    fun report() = FrameReport(
        submitCostNs = cost.map { it.toULong() },
        releasedNs = released.map { it.toULong() },
        notTaken = notTaken.toUInt(),
        firstRefusal = (if (firstRefusal < 0) 0 else firstRefusal).toUInt(),
        pendingFences = pendingFences.toUInt(),
        note = note.ifEmpty { "ok" },
    )

    private fun record(seq: Int, t0: Long, status: IstmoSubmitStatus) {
        cost[seq] = System.nanoTime() - t0
        if (!status.isTaken) {
            notTaken++
            if (firstRefusal < 0) firstRefusal = status.code
        }
    }

    private fun pause() {
        if (gapNs > 0) LockSupport.parkNanos(gapNs)
    }

    fun bytes() {
        val buf = ByteArray(size)
        for (seq in 0 until count) {
            val t0 = System.nanoTime()
            record(seq, t0, sender.submitBytes(buf, FrameMeta(seq.toUInt(), t0.toULong())))
            pause()
        }
    }

    fun slots() {
        check(sender.openSlotPool(size, 4)) { "openSlotPool($size, 4) refused" }
        for (seq in 0 until count) {
            var slot = sender.acquireSlot()
            while (slot == null) {
                LockSupport.parkNanos(20_000)
                slot = sender.acquireSlot()
            }
            slot.buffer.putInt(0, seq)
            val t0 = System.nanoTime()
            record(seq, t0, sender.submit(slot, size, FrameMeta(seq.toUInt(), t0.toULong())))
            pause()
        }
    }

    /** One pooled buffer lent to Rust; closing it means Rust let go. */
    private inner class Lease(private val index: Int, private val seq: Int, private val free: ArrayBlockingQueue<Int>, private val done: CountDownLatch) :
        AutoCloseable {
        override fun close() {
            released[seq] = System.nanoTime()
            free.offer(index)
            done.countDown()
        }
    }

    fun hardwareBuffers() {
        val (w, h) = rgbaShape(size)
        val usage = HardwareBuffer.USAGE_CPU_READ_RARELY or HardwareBuffer.USAGE_CPU_WRITE_RARELY or
            HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE
        val pool = Array(4) { HardwareBuffer.create(w, h, HardwareBuffer.RGBA_8888, 1, usage) }
        val free = ArrayBlockingQueue<Int>(pool.size).apply { pool.indices.forEach { offer(it) } }
        val done = CountDownLatch(count)
        try {
            for (seq in 0 until count) {
                val index = free.poll(2, TimeUnit.SECONDS) ?: error("no buffer came back in 2 s (seq $seq)")
                val t0 = System.nanoTime()
                record(seq, t0, sender.submit(pool[index], FrameMeta(seq.toUInt(), t0.toULong()), Lease(index, seq, free, done)))
                pause()
            }
            if (!done.await(5, TimeUnit.SECONDS)) note = "${done.count} owners never closed"
        } finally {
            pool.forEach { it.close() }
        }
    }

    fun images() {
        if (Build.VERSION.SDK_INT < 29) {
            note = "ImageReader with usage needs API 29"
            return
        }
        val (w, h) = rgbaShape(size)
        val reader = ImageReader.newInstance(
            w, h, PixelFormat.RGBA_8888, 4,
            HardwareBuffer.USAGE_GPU_SAMPLED_IMAGE or HardwareBuffer.USAGE_CPU_READ_RARELY or
                HardwareBuffer.USAGE_GPU_COLOR_OUTPUT,
        )
        val thread = HandlerThread("bench-image").apply { start() }
        val delivered = AtomicInteger(0)
        val all = CountDownLatch(count)
        reader.setOnImageAvailableListener({ r ->
            val image = try {
                r.acquireNextImage()
            } catch (e: IllegalStateException) {
                // maxImages already acquired: Rust never let go of them.
                note = "acquireNextImage: ${e.message}"
                null
            } ?: return@setOnImageAvailableListener
            val seq = delivered.getAndIncrement()
            if (seq >= count) {
                image.close()
                return@setOnImageAvailableListener
            }
            if (Build.VERSION.SDK_INT >= 33) {
                val fence = image.fence
                if (fence.isValid && fence.signalTime == android.hardware.SyncFence.SIGNAL_TIME_PENDING) pendingFences++
            }
            val t0 = System.nanoTime()
            record(seq, t0, sender.submit(image, FrameMeta(seq.toUInt(), t0.toULong())))
            all.countDown()
        }, Handler(thread.looper))
        val surface = reader.surface
        try {
            for (seq in 0 until count) {
                val canvas = surface.lockHardwareCanvas()
                canvas.drawColor(if (seq % 2 == 0) Color.RED else Color.BLUE)
                surface.unlockCanvasAndPost(canvas)
                pause()
                // Never run more than maxImages ahead of the listener.
                val deadline = SystemClock.elapsedRealtime() + 2_000
                while (seq - delivered.get() >= 3 && SystemClock.elapsedRealtime() < deadline) {
                    LockSupport.parkNanos(50_000)
                }
            }
            if (!all.await(10, TimeUnit.SECONDS)) {
                note = (if (note.isEmpty()) "" else "$note; ") + "only ${count - all.count} of $count images arrived"
            }
        } finally {
            reader.setOnImageAvailableListener(null, null)
            thread.quitSafely()
            surface.release()
            reader.close()
        }
    }
}
