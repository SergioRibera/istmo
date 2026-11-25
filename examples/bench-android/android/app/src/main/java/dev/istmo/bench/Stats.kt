package dev.istmo.bench

/** Order statistics of one cell's measured samples (warm-up excluded), in ns. */
data class Stats(
    val n: Int,
    val p50: Long,
    val p95: Long,
    val p99: Long,
    val min: Long,
    val max: Long,
    val mean: Long,
) {
    companion object {
        /** Summarise `samples[from until to]`; zero values are skipped when [skipZero]. */
        fun of(samples: LongArray, from: Int = 0, to: Int = samples.size, skipZero: Boolean = false): Stats {
            val slice = samples.copyOfRange(from.coerceAtMost(to), to).let { s ->
                if (skipZero) s.filter { it > 0 }.toLongArray() else s
            }
            if (slice.isEmpty()) return Stats(0, 0, 0, 0, 0, 0, 0)
            slice.sort()
            fun pct(p: Int) = slice[((slice.size - 1) * p) / 100]
            return Stats(
                n = slice.size,
                p50 = pct(50),
                p95 = pct(95),
                p99 = pct(99),
                min = slice.first(),
                max = slice.last(),
                mean = slice.sum() / slice.size,
            )
        }
    }

    fun render(): String =
        "n=%5d p50=%9d p95=%9d p99=%9d min=%9d max=%9d mean=%9d".format(n, p50, p95, p99, min, max, mean)
}
