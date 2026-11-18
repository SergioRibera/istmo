package dev.istmo.bench

/** JNI bridge to the Rust benchmark harness. */
object BenchNative {
    init {
        System.loadLibrary("bench_android")
    }

    /** Dispatch mode mirror of the Rust enum. */
    enum class Mode(val id: Int) {
        Isolated(0),
        Inline(1),
        Typed(2),
    }

    /**
     * Run `iters` round-trip calls through `mode` with a payload of
     * `payloadLen` bytes. Returns the total wall time spent in ns.
     * Negative values indicate an invalid argument.
     */
    @JvmStatic
    external fun runIters(mode: Int, iters: Long, payloadLen: Int): Long
}
