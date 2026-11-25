package dev.istmo.bench

/**
 * JNI bridge to the Rust half of the benchmark (`libbench_android.so`).
 * The library is loaded by [BenchActivity] before anything here runs.
 */
object BenchNative {
    /** Rust-driven cells; ids mirror `cells::Cell`. */
    enum class Cell(val id: Int) {
        IsolatedRaw(0),
        InlineRaw(1),
        Typed(2),
        KotlinEcho(3),
        KotlinEchoRaw(4),
        KotlinStreamPaced(5),
        KotlinStreamBurst(6),
        FlumePingPong(12),
        CondvarPingPong(13),
        FutexPingPong(14),
        SpinThenPark(15),
        ThreadSpawnStart(16),
        ThreadSpawnJoin(17),
        ClockOverhead(18),
    }

    /** `CLOCK_MONOTONIC` as Rust reads it. */
    @JvmStatic external fun nowNs(): Long

    /** The CPU the calling thread runs on. */
    @JvmStatic external fun currentCpu(): Int

    /** `name@cpu` of the threads that take part in a crossing. */
    @JvmStatic external fun threadMap(): String

    /** `cpu:kHz` of every CPU, `-` when the sandbox hides it. */
    @JvmStatic external fun cpuFreqs(): String

    /** Restrict every thread of the process to [mask] (`0` = all CPUs). */
    @JvmStatic external fun pinProcess(mask: Long): Int

    @JvmStatic external fun startSpinners(count: Int, mask: Long)

    @JvmStatic external fun stopSpinners()

    /** Run a Rust-driven cell; returns the samples written or a negative code. */
    @JvmStatic external fun runCell(cell: Int, samples: Int, size: Int, out: LongArray, extra: LongArray): Int

    /** Run a frame-sink cell; returns the producer/consumer note. */
    @JvmStatic external fun runFrames(
        path: Int,
        count: Int,
        size: Int,
        gapUs: Int,
        latency: LongArray,
        release: LongArray,
        submit: LongArray,
        counts: LongArray,
    ): String

    /** Rust → Kotlin JNI baselines: 0 call, 1 call with `byte[size]`, 2 attach + detach. */
    @JvmStatic external fun jniCallbacks(kind: Int, samples: Int, size: Int, out: LongArray): Int

    /** Legacy batch mode: total ns of [iters] calls (0 Isolated, 1 Inline, 2 Typed). */
    @JvmStatic external fun runIters(mode: Int, iters: Long, payloadLen: Int): Long

    @JvmStatic external fun noopJni(): Long

    @JvmStatic external fun noopJniBytes(payload: ByteArray): Long

    @JvmStatic external fun setProbing(on: Boolean)

    /** Turn the runtime's spin-before-park on or off. */
    @JvmStatic external fun setSpinning(on: Boolean)

    /** One probed Rust → Kotlin → Rust echo; see `jni.rs` for the layout. */
    @JvmStatic external fun probeRustToKotlin(size: Int, out: LongArray): Int

    /** Last Rust host body stamps plus the transport stamps. */
    @JvmStatic external fun readStamps(out: LongArray): Int

    /** Target of the Rust → Kotlin `CallStaticVoidMethod` baseline. */
    @JvmStatic fun kNoop() {}

    /** Target of the Rust → Kotlin byte-array baseline. */
    @JvmStatic
    @Suppress("UNUSED_PARAMETER")
    fun kNoopBytes(payload: ByteArray) {}
}
