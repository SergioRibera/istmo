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
        // Pure JNI roundtrip + optional ByteArray decode. These two
        // isolate the JNI machinery istmo pays on top of its own
        // dispatch cost; Kotlin times them directly instead of through
        // `runIters` because there is no istmo logic to batch.
        JniNoop(100),
        JniNoopBytes(101),
    }

    /**
     * Run `iters` round-trip calls through `mode` with a payload of
     * `payloadLen` bytes. Returns the total wall time spent in ns.
     * Negative values indicate an invalid argument.
     */
    @JvmStatic
    external fun runIters(mode: Int, iters: Long, payloadLen: Int): Long

    /** Pure JNI entry+exit, no payload. */
    @JvmStatic
    external fun noopJni(): Long

    /** JNI entry+exit plus `convert_byte_array` on the supplied payload. */
    @JvmStatic
    external fun noopJniBytes(payload: ByteArray): Long
}
