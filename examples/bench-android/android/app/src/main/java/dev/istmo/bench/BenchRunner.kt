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

    fun run(mode: BenchNative.Mode, payloadBytes: Int): BenchResult {
        // Warm-up: one batch discarded, lets the JIT settle and the
        // first-call cold-cache cost fall out.
        BenchNative.runIters(mode.id, BATCH_ITERS, payloadBytes)

        val samples = LongArray(BATCHES)
        var minNs = Long.MAX_VALUE
        var maxNs = Long.MIN_VALUE
        var totalNs = 0L
        for (i in 0 until BATCHES) {
            val ns = BenchNative.runIters(mode.id, BATCH_ITERS, payloadBytes)
            require(ns >= 0) { "runIters returned $ns for mode=$mode" }
            samples[i] = ns / BATCH_ITERS
            totalNs += ns
            minNs = min(minNs, samples[i])
            maxNs = max(maxNs, samples[i])
        }
        samples.sort()
        return BenchResult(
            mode = mode,
            payloadBytes = payloadBytes,
            iterations = BATCH_ITERS * BATCHES,
            totalNs = totalNs,
            p50Ns = samples[samples.size / 2],
            p95Ns = samples[(samples.size * 95) / 100],
            p99Ns = samples[(samples.size * 99) / 100],
            minNs = minNs,
            maxNs = maxNs,
        )
    }

    fun runFullMatrix(): List<BenchResult> = buildList {
        for (mode in BenchNative.Mode.values()) {
            for (size in PAYLOAD_SIZES) {
                add(run(mode, size))
            }
        }
    }
}
