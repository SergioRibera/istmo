package dev.istmo.bench

import kotlin.math.max
import kotlin.math.min

/** One measurement line: mode + payload size → statistics in ns. */
data class BenchResult(
    val mode: BenchNative.Mode,
    val payloadBytes: Int,
    val iterations: Long,
    val totalNs: Long,
    val p50Ns: Long,
    val p95Ns: Long,
    val p99Ns: Long,
    val minNs: Long,
    val maxNs: Long,
) {
    val perCallNs: Long = if (iterations == 0L) 0 else totalNs / iterations
}

/** Repeatedly measure a mode and carve out percentile stats from the batch. */
object BenchRunner {
    private const val BATCH_ITERS = 1_000L
    private const val BATCHES = 100

    val PAYLOAD_SIZES = intArrayOf(0, 64, 1024, 65_536)

    fun run(mode: BenchNative.Mode, payloadBytes: Int): BenchResult = when (mode) {
        BenchNative.Mode.JniNoop -> runJniNoop(payloadBytes)
        BenchNative.Mode.JniNoopBytes -> runJniNoopBytes(payloadBytes)
        else -> runIstmo(mode, payloadBytes)
    }

    private fun runIstmo(mode: BenchNative.Mode, payloadBytes: Int): BenchResult {
        // Warm-up: one batch discarded, lets the JIT settle and the
        // first-call cold-cache cost fall out.
        BenchNative.runIters(mode.id, BATCH_ITERS, payloadBytes)
        val samples = LongArray(BATCHES)
        for (i in 0 until BATCHES) {
            val ns = BenchNative.runIters(mode.id, BATCH_ITERS, payloadBytes)
            require(ns >= 0) { "runIters returned $ns for mode=$mode" }
            samples[i] = ns / BATCH_ITERS
        }
        return summarise(mode, payloadBytes, samples)
    }

    private fun runJniNoop(payloadBytes: Int): BenchResult {
        // payloadBytes is ignored for the no-argument baseline; still
        // carried in the result so the rendered table lines up with the
        // other modes.
        // Warm-up.
        repeat(1000) { BenchNative.noopJni() }

        val samples = LongArray(BATCHES)
        for (i in 0 until BATCHES) {
            val start = System.nanoTime()
            for (j in 0 until BATCH_ITERS.toInt()) {
                BenchNative.noopJni()
            }
            samples[i] = (System.nanoTime() - start) / BATCH_ITERS
        }
        return summarise(BenchNative.Mode.JniNoop, payloadBytes, samples)
    }

    private fun runJniNoopBytes(payloadBytes: Int): BenchResult {
        val payload = ByteArray(payloadBytes) { (it and 0xff).toByte() }
        // Warm-up.
        repeat(1000) { BenchNative.noopJniBytes(payload) }

        val samples = LongArray(BATCHES)
        for (i in 0 until BATCHES) {
            val start = System.nanoTime()
            for (j in 0 until BATCH_ITERS.toInt()) {
                BenchNative.noopJniBytes(payload)
            }
            samples[i] = (System.nanoTime() - start) / BATCH_ITERS
        }
        return summarise(BenchNative.Mode.JniNoopBytes, payloadBytes, samples)
    }

    private fun summarise(
        mode: BenchNative.Mode,
        payloadBytes: Int,
        samples: LongArray,
    ): BenchResult {
        var minNs = Long.MAX_VALUE
        var maxNs = Long.MIN_VALUE
        var totalNs = 0L
        for (s in samples) {
            minNs = min(minNs, s)
            maxNs = max(maxNs, s)
            totalNs += s * BATCH_ITERS
        }
        samples.sort()
        return BenchResult(
            mode = mode,
            payloadBytes = payloadBytes,
            iterations = BATCH_ITERS * samples.size,
            totalNs = totalNs,
            p50Ns = samples[samples.size / 2],
            p95Ns = samples[(samples.size * 95) / 100],
            p99Ns = samples[(samples.size * 99) / 100],
            minNs = minNs,
            maxNs = maxNs,
        )
    }

    fun runFullMatrix(): List<BenchResult> = buildList {
        // Baselines first — `JniNoop` payload size is a no-op but keep
        // the 0-byte row so the table has a matching cell.
        add(run(BenchNative.Mode.JniNoop, 0))
        for (size in PAYLOAD_SIZES) {
            add(run(BenchNative.Mode.JniNoopBytes, size))
        }
        for (mode in listOf(
            BenchNative.Mode.Isolated,
            BenchNative.Mode.Inline,
            BenchNative.Mode.Typed,
        )) {
            for (size in PAYLOAD_SIZES) {
                add(run(mode, size))
            }
        }
    }
}
