package dev.istmo.bench

import android.os.Build
import android.os.Handler
import android.os.HandlerThread
import dev.istmo.bench.gen.RustBenchClient
import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoBytesOut
import dev.istmo.runtime.IstmoRuntime
import java.io.File
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.locks.LockSupport
import kotlin.concurrent.thread
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.collect
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withContext

/**
 * Drives the whole matrix and prints one line per cell through [out].
 *
 * Every per-call cell runs [warmup] discarded iterations then [iters]
 * measured ones, each timed on its own (`first` = the very first
 * warm-up sample, i.e. the cold call). Threads' last CPUs are appended
 * after `|`.
 */
class BenchRunner(
    private val out: (String) -> Unit,
    private val iters: Int = 1000,
    private val warmup: Int = 100,
) {
    private val total = iters + warmup
    private val sizes = intArrayOf(0, 64, 1024, 65_536)
    private val rustClient = RustBenchClient()

    private fun line(section: String, name: String, size: Int, cond: String, samples: LongArray, note: String = "") {
        val stats = Stats.of(samples, from = warmup.coerceAtMost(samples.size))
        val first = if (samples.isNotEmpty()) samples[0] else 0
        out(
            "%-8s %-22s %8d %-8s %s first=%9d %s| %s".format(
                section, name, size, cond, stats.render(), first, if (note.isEmpty()) "" else "$note ", BenchNative.threadMap(),
            ),
        )
    }

    // ---- environment ---------------------------------------------------

    fun environment() {
        out("device: ${Build.MANUFACTURER} ${Build.MODEL} sdk=${Build.VERSION.SDK_INT} release=${Build.VERSION.RELEASE}")
        if (Build.VERSION.SDK_INT >= 31) out("soc: ${Build.SOC_MANUFACTURER} ${Build.SOC_MODEL}")
        val parts = File("/proc/cpuinfo").readLines().filter { it.startsWith("CPU part") }.map { it.substringAfter(":").trim() }
        for (cpu in parts.indices) {
            val base = "/sys/devices/system/cpu/cpu$cpu"
            out(
                "cpu$cpu part=${parts[cpu]} capacity=${read("$base/cpu_capacity")} max_khz=${read("$base/cpufreq/cpuinfo_max_freq")} " +
                    "governor=${read("$base/cpufreq/scaling_governor")}",
            )
        }
        out("freqs: ${BenchNative.cpuFreqs()}")
        out("masks: little=0x%x big=0x%x".format(littleMask, bigMask))
    }

    private fun read(path: String): String = runCatching { File(path).readText().trim() }.getOrDefault("?")

    private val capacities: List<Int> by lazy {
        (0 until Runtime.getRuntime().availableProcessors()).map { cpu ->
            read("/sys/devices/system/cpu/cpu$cpu/cpu_capacity").toIntOrNull()
                ?: read("/sys/devices/system/cpu/cpu$cpu/cpufreq/cpuinfo_max_freq").toIntOrNull()
                ?: 0
        }
    }
    private val littleMask: Long by lazy { maskWhere { cap, max -> cap * 2 <= max } }
    private val bigMask: Long by lazy { maskWhere { cap, max -> cap * 10 >= max * 9 } }

    private fun maskWhere(pick: (Int, Int) -> Boolean): Long {
        val max = capacities.maxOrNull() ?: 0
        var mask = 0L
        capacities.forEachIndexed { cpu, cap -> if (max > 0 && pick(cap, max)) mask = mask or (1L shl cpu) }
        return mask
    }

    /** `System.nanoTime()` and Rust's `CLOCK_MONOTONIC` must share a base. */
    fun clockCheck() {
        var outside = 0
        var widest = 0L
        var offsetMax = 0L
        repeat(10_000) {
            val k0 = System.nanoTime()
            val r = BenchNative.nowNs()
            val k1 = System.nanoTime()
            if (r < k0 || r > k1) {
                outside++
                offsetMax = maxOf(offsetMax, maxOf(k0 - r, r - k1))
            }
            widest = maxOf(widest, k1 - k0)
        }
        out("clock: nanoTime vs CLOCK_MONOTONIC: $outside/10000 outside the bracket (max off $offsetMax ns), widest bracket $widest ns")
    }

    // ---- per-call matrix -------------------------------------------------

    private fun rustCell(section: String, cell: BenchNative.Cell, size: Int, cond: String = "idle", name: String = cell.name): LongArray {
        val samples = LongArray(total)
        val extra = LongArray(4)
        val n = BenchNative.runCell(cell.id, total, size, samples, extra)
        if (n < 0) {
            out("%-8s %-22s %8d %-8s FAILED code=%d".format(section, name, size, cond, n))
            return LongArray(0)
        }
        val note = when (cell) {
            BenchNative.Cell.KotlinStreamBurst, BenchNative.Cell.KotlinStreamPaced ->
                "items/s=%d first_item_ns=%d cpu=%d/%d".format(
                    if (extra[0] > 0) n * 1_000_000_000L / extra[0] else 0,
                    extra[1],
                    extra[2], // consumer (Rust); -1 unknown
                    extra[3], // producer (Kotlin)
                )
            else -> ""
        }
        line(section, name, size, cond, samples.copyOf(n), note)
        return samples
    }

    private fun timed(body: () -> Unit): LongArray {
        val samples = LongArray(total)
        for (i in 0 until total) {
            val t0 = System.nanoTime()
            body()
            samples[i] = System.nanoTime() - t0
        }
        return samples
    }

    private fun timedSuspend(body: suspend () -> Unit): LongArray = runBlocking {
        val samples = LongArray(total)
        for (i in 0 until total) {
            val t0 = System.nanoTime()
            body()
            samples[i] = System.nanoTime() - t0
        }
        samples
    }

    private fun encodeBytes(payload: ByteArray): ByteArray =
        IstmoBytesOut().also { Bincode.writeBytes(it, payload) }.toExactArray()

    /** Kotlin → Rust crossings (Kotlin times them). */
    private fun kotlinToRust(cond: String, size: Int) {
        val payload = ByteArray(size) { it.toByte() }
        line("kt->rs", "RustEcho(client,Isol)", size, cond, timedSuspend { rustClient.echo(payload) })
        line("kt->rs", "RawCall(Inline)", size, cond, timedSuspend { IstmoRuntime.call("bench.inline", "echo", payload) })
        line("kt->rs", "RawCall(Isolated)", size, cond, timedSuspend { IstmoRuntime.call("bench.isolated", "echo", payload) })
    }

    /** Rust → Kotlin crossings (Rust times them). */
    private fun rustToKotlin(cond: String, size: Int) {
        rustCell("rs->kt", BenchNative.Cell.KotlinEcho, size, cond, "KotlinEcho(client)")
        rustCell("rs->kt", BenchNative.Cell.KotlinEchoRaw, size, cond, "KotlinEcho(raw)")
    }

    fun matrix() {
        out("== per-call matrix (warm-up $warmup, measured $iters)")
        line("base", "ClockOverhead", 0, "idle", rustCellSamples(BenchNative.Cell.ClockOverhead, 0))
        line("base", "kt nanoTime pair", 0, "idle", timed {})
        line("jni", "kt->rs noop", 0, "idle", timed { BenchNative.noopJni() })
        for (size in sizes) {
            val payload = ByteArray(size)
            line("jni", "kt->rs noop(byte[])", size, "idle", timed { BenchNative.noopJniBytes(payload) })
        }
        jniCallback("rs->kt CallStatic", 0, 0)
        for (size in sizes) jniCallback("rs->kt Call(new byte[])", 1, size)
        jniCallback("rs->kt attach+detach", 2, 0)
        for (size in sizes) {
            rustCell("rs", BenchNative.Cell.IsolatedRaw, size)
            rustCell("rs", BenchNative.Cell.InlineRaw, size)
            rustCell("rs", BenchNative.Cell.Typed, size)
        }
        for (size in sizes) {
            rustToKotlin("idle", size)
            kotlinToRust("idle", size)
        }
        out("freqs: ${BenchNative.cpuFreqs()}")
    }

    private fun rustCellSamples(cell: BenchNative.Cell, size: Int): LongArray {
        val samples = LongArray(total)
        val n = BenchNative.runCell(cell.id, total, size, samples, LongArray(4))
        return if (n < 0) LongArray(0) else samples.copyOf(n)
    }

    private fun jniCallback(name: String, kind: Int, size: Int) {
        val samples = LongArray(total)
        val n = BenchNative.jniCallbacks(kind, total, size, samples)
        if (n < 0) out("jni      $name FAILED code=$n") else line("jni", name, size, "idle", samples.copyOf(n))
    }

    // ---- streams ---------------------------------------------------------

    fun streams() {
        out("== streams ($iters items after $warmup warm-up; paced = 200 us gap, burst = back to back)")
        for (size in intArrayOf(64, 1024, 65_536)) {
            rustCell("stream", BenchNative.Cell.KotlinStreamPaced, size, name = "kt->rs paced")
            rustCell("stream", BenchNative.Cell.KotlinStreamBurst, size, name = "kt->rs burst")
            rustStream(size, 200, "rs->kt paced")
            rustStream(size, 0, "rs->kt burst")
        }
    }

    private fun rustStream(size: Int, gapUs: Int, name: String, cond: String = "idle") {
        val latency = LongArray(total)
        var i = 0
        val t0 = System.nanoTime()
        var firstItem = 0L
        try {
            runBlocking {
                rustClient.ticks(total.toUInt(), size.toUInt(), gapUs.toUInt()).collect { item ->
                    val now = System.nanoTime()
                    if (i == 0) firstItem = now - t0
                    var sent = 0L
                    for (b in 7 downTo 0) sent = (sent shl 8) or (item[b].toLong() and 0xff)
                    if (i < total) latency[i] = now - sent
                    i++
                }
            }
        } catch (e: Throwable) {
            out("stream   $name $size FAILED after $i items: $e")
            return
        }
        val elapsed = System.nanoTime() - t0
        line("stream", name, size, cond, latency.copyOf(minOf(i, total)), "items/s=%d first_item_ns=%d".format(i * 1_000_000_000L / elapsed, firstItem))
    }

    // ---- wake-up baselines -----------------------------------------------

    fun wakeups(cond: String = "idle") {
        out("== thread hand-off baselines ($cond); samples are round trips")
        rustCell("wake", BenchNative.Cell.FutexPingPong, 0, cond, "futex")
        rustCell("wake", BenchNative.Cell.CondvarPingPong, 0, cond, "condvar")
        rustCell("wake", BenchNative.Cell.FlumePingPong, 0, cond, "flume")
        for (spin in intArrayOf(5, 20, 50)) rustCell("wake", BenchNative.Cell.SpinThenPark, spin, cond, "flume spin${spin}us")
        rustCell("wake", BenchNative.Cell.ThreadSpawnStart, 0, cond, "spawn->start")
        rustCell("wake", BenchNative.Cell.ThreadSpawnJoin, 0, cond, "spawn->join")
        line("wake", "kt park/unpark", 0, cond, parkPingPong())
        line("wake", "kt Looper post", 0, cond, looperRoundTrip())
        line("wake", "kt withContext(Default)", 0, cond, timedSuspend { withContext(Dispatchers.Default) {} })
    }

    private fun parkPingPong(): LongArray {
        // 0: idle, 1: partner's move, 2: caller's move, -1: stop.
        val turn = AtomicInteger(0)
        val caller = Thread.currentThread()
        val partner = thread(name = "bench-kpartner") {
            while (true) {
                when (turn.get()) {
                    -1 -> return@thread
                    1 -> {
                        turn.set(2)
                        LockSupport.unpark(caller)
                    }
                    else -> LockSupport.park()
                }
            }
        }
        val samples = timed {
            turn.set(1)
            LockSupport.unpark(partner)
            while (turn.get() != 2) LockSupport.park()
            turn.set(0)
        }
        turn.set(-1)
        LockSupport.unpark(partner)
        partner.join()
        return samples
    }

    private fun looperRoundTrip(): LongArray {
        val handlerThread = HandlerThread("bench-looper").apply { start() }
        val handler = Handler(handlerThread.looper)
        val caller = Thread.currentThread()
        val done = AtomicBoolean(false)
        val samples = timed {
            done.set(false)
            handler.post {
                done.set(true)
                LockSupport.unpark(caller)
            }
            while (!done.get()) LockSupport.park()
        }
        handlerThread.quitSafely()
        return samples
    }

    // ---- frame sinks -----------------------------------------------------

    fun frames() {
        out("== frame sinks (Kotlin producer → Rust FrameSink queue(4); $iters frames after 50 warm-up)")
        val shapes = listOf(4096 to 300, 1_048_576 to 1_000, 1920 * 1080 * 4 to 3_000)
        for (path in FramePath.values()) {
            for ((size, gapUs) in shapes) frameCell(path, size, gapUs)
        }
    }

    private fun frameCell(path: FramePath, size: Int, gapUs: Int, cond: String = "idle") {
        val count = iters + 50
        val latency = LongArray(count)
        val release = LongArray(count)
        val submit = LongArray(count)
        val counts = LongArray(4)
        val note = BenchNative.runFrames(path.id, count, size, gapUs, latency, release, submit, counts)
        val tail = "received=${counts[0]} evicted=${counts[1]} unsound=${counts[2]} not_taken=${counts[3]} $note"
        val name = "frames ${path.name}"
        out(
            "%-8s %-22s %8d %-8s %s | %s".format(
                "frames", "$name submit->recv", size, cond, Stats.of(latency, from = 50, skipZero = true).render(), tail,
            ),
        )
        out("%-8s %-22s %8d %-8s %s".format("frames", "$name submit cost", size, cond, Stats.of(submit, from = 50, skipZero = true).render()))
        if (path == FramePath.HardwareBuffer) {
            out("%-8s %-22s %8d %-8s %s".format("frames", "$name drop->closed", size, cond, Stats.of(release, from = 50, skipZero = true).render()))
        }
    }

    // ---- sensitivity -----------------------------------------------------

    fun sensitivity() {
        val cpus = Runtime.getRuntime().availableProcessors()
        val conditions = listOf("little", "big", "busy1", "busyAll")
        for (cond in conditions) {
            when (cond) {
                "little" -> out("pinned ${BenchNative.pinProcess(littleMask)} threads to 0x%x".format(littleMask))
                "big" -> out("pinned ${BenchNative.pinProcess(bigMask)} threads to 0x%x".format(bigMask))
                "busy1" -> BenchNative.startSpinners(1, 0)
                "busyAll" -> BenchNative.startSpinners(cpus, 0)
            }
            out("== sensitivity: $cond")
            try {
                rustToKotlin(cond, 64)
                kotlinToRust(cond, 64)
                rustCell("rs", BenchNative.Cell.IsolatedRaw, 64, cond)
                rustCell("rs", BenchNative.Cell.InlineRaw, 64, cond)
                rustCell("wake", BenchNative.Cell.FlumePingPong, 0, cond, "flume")
                rustCell("wake", BenchNative.Cell.SpinThenPark, 20, cond, "flume spin20us")
                rustCell("wake", BenchNative.Cell.ThreadSpawnStart, 0, cond, "spawn->start")
                line("wake", "kt withContext(Default)", 0, cond, timedSuspend { withContext(Dispatchers.Default) {} })
                out("freqs: ${BenchNative.cpuFreqs()}")
            } finally {
                BenchNative.pinProcess(0)
                BenchNative.stopSpinners()
            }
        }
    }

    // ---- spin-before-park A/B -----------------------------------------

    /**
     * The waits that still cross threads (stream items, frames, the pump,
     * blocking hosts), with the runtime's spin-before-park on and off,
     * idle and with every core busy.
     */
    fun spinAB() {
        val cpus = Runtime.getRuntime().availableProcessors()
        for (busy in listOf(false, true)) {
            if (busy) BenchNative.startSpinners(cpus, 0)
            try {
                for (spin in listOf(true, false)) {
                    BenchNative.setSpinning(spin)
                    val cond = (if (busy) "busy" else "idle") + (if (spin) "/sp1" else "/sp0")
                    out("== spin A/B: $cond")
                    rustCell("stream", BenchNative.Cell.KotlinStreamPaced, 64, cond, "kt->rs paced")
                    rustCell("stream", BenchNative.Cell.KotlinStreamBurst, 64, cond, "kt->rs burst")
                    rustStream(64, 200, "rs->kt paced", cond)
                    rustStream(64, 0, "rs->kt burst", cond)
                    frameCell(FramePath.Bytes, 4096, 300, cond)
                    rustToKotlin(cond, 64)
                    kotlinToRust(cond, 64)
                    rustCell("rs", BenchNative.Cell.IsolatedRaw, 64, cond)
                }
            } finally {
                BenchNative.setSpinning(true)
                BenchNative.stopSpinners()
            }
        }
    }

    // ---- legacy ------------------------------------------------------------

    /** The original matrix, unchanged: per-call means of 100 batches of 1000. */
    fun legacy() {
        out("== legacy matrix (p50.. over 100 batch means of 1000 calls)")
        out("mode      payload    p50       p95       p99       min       max")
        val batches = 100
        val batch = 1_000L
        fun row(mode: String, size: Int, samples: LongArray) {
            val s = Stats.of(samples)
            out("%-8s  %7dB  %6d ns %6d ns %6d ns %6d ns %6d ns".format(mode, size, s.p50, s.p95, s.p99, s.min, s.max))
        }
        fun batchMeans(body: () -> Long): LongArray {
            body()
            return LongArray(batches) { body() / batch }
        }
        row("JniNoop", 0, batchMeans { val t0 = System.nanoTime(); for (j in 0 until batch) BenchNative.noopJni(); System.nanoTime() - t0 })
        for (size in sizes) {
            val payload = ByteArray(size)
            row("JniNoopBytes", size, batchMeans { val t0 = System.nanoTime(); for (j in 0 until batch) BenchNative.noopJniBytes(payload); System.nanoTime() - t0 })
        }
        for ((mode, id) in listOf("Isolated" to 0, "Inline" to 1, "Typed" to 2)) {
            for (size in sizes) row(mode, size, batchMeans { BenchNative.runIters(id, batch, size) })
        }
    }

    // ---- per-hop breakdown (probe launch only) -------------------------

    private class Chain(val title: String) {
        val stamps = LinkedHashMap<String, LongArray>()
        fun at(name: String, n: Int): LongArray = stamps.getOrPut(name) { LongArray(n) }
    }

    private fun renderChain(chain: Chain, size: Int, stages: List<Triple<String, String, String>>, extra: List<Triple<String, String, String>>) {
        out("== breakdown: ${chain.title} size=$size (p50 / p95 / mean ns per stage; means add up to the total mean)")
        fun stage(label: String, from: String, to: String, info: Boolean) {
            val a = chain.stamps[from] ?: return
            val b = chain.stamps[to] ?: return
            val d = LongArray(a.size) { b[it] - a[it] }
            val negatives = d.drop(warmup).count { it < 0 }
            val s = Stats.of(d, from = warmup)
            out(
                "%-8s %-52s p50=%9d p95=%9d mean=%9d%s".format(
                    if (info) "  info" else "  stage", "$label [$from→$to]", s.p50, s.p95, s.mean,
                    if (negatives > 0) " negative=$negatives" else "",
                ),
            )
        }
        stages.forEach { (label, from, to) -> stage(label, from, to, false) }
        val first = stages.first().second
        val last = stages.last().third
        stage("TOTAL", first, last, false)
        extra.forEach { (label, from, to) -> stage(label, from, to, true) }
        out("   threads: ${BenchNative.threadMap()}")
    }

    private val transport = listOf("PD", "PCK", "PKR", "SRE", "SRD", "SCE", "SCD", "SEE", "DCK", "DKR")

    fun breakdown() {
        Probe.enabled = true
        BenchNative.setProbing(true)
        try {
            for (size in intArrayOf(64, 65_536)) {
                breakdownRustToKotlin(size)
                breakdownKotlinToRust(size, generated = true)
                breakdownKotlinToRust(size, generated = false)
            }
        } finally {
            Probe.enabled = false
            BenchNative.setProbing(false)
        }
    }

    private fun breakdownRustToKotlin(size: Int) {
        val chain = Chain("Rust → Kotlin → Rust (KotlinBench.echo, generated dispatcher)")
        val rs = LongArray(5 + transport.size)
        for (i in 0 until total) {
            if (BenchNative.probeRustToKotlin(size, rs) < 0) {
                out("breakdown rs->kt FAILED")
                return
            }
            Probe.settle(Probe.ON_CALL_ENTER, Probe.ON_CALL_EXIT)
            listOf("R0", "R1", "R2", "R3", "R4").forEachIndexed { k, name -> chain.at(name, total)[i] = rs[k] }
            transport.forEachIndexed { k, name -> chain.at(name, total)[i] = rs[5 + k] }
            chain.at("OCE", total)[i] = Probe.read(Probe.ON_CALL_ENTER)
            chain.at("OCX", total)[i] = Probe.read(Probe.ON_CALL_EXIT)
            chain.at("HE", total)[i] = Probe.read(Probe.HANDLER_ENTER)
            chain.at("BE", total)[i] = Probe.read(Probe.BACKEND_ENTER)
            chain.at("BX", total)[i] = Probe.read(Probe.BACKEND_EXIT)
            chain.at("HX", total)[i] = Probe.read(Probe.HANDLER_EXIT)
        }
        renderChain(
            chain, size,
            listOf(
                Triple("encode (Rust codec)", "R0", "R1"),
                Triple("Runtime::call: route, names, byte[]", "R1", "DCK"),
                Triple("JNI Rust→Kotlin CallStaticObject (caller thread)", "DCK", "OCE"),
                Triple("onCallDirect: job + start in place", "OCE", "HE"),
                Triple("decode (Kotlin dispatcher)", "HE", "BE"),
                Triple("backend body", "BE", "BX"),
                Triple("reply encode (Kotlin dispatcher)", "BX", "HX"),
                Triple("return to Rust (reply as return value)", "HX", "DKR"),
                Triple("reply byte[] copy + complete the call", "DKR", "R2"),
                Triple("recv (answer already there)", "R2", "R3"),
                Triple("decode (Rust codec)", "R3", "R4"),
            ),
            listOf(
                Triple("onCallDirect body", "OCE", "OCX"),
            ),
        )
    }

    private fun breakdownKotlinToRust(size: Int, generated: Boolean) {
        val title = if (generated) "Kotlin → Rust → Kotlin (RustBench.echo: generated client, generated host)"
        else "Kotlin → Rust → Kotlin (raw IstmoRuntime.call, Inline host)"
        val chain = Chain(title)
        val payload = ByteArray(size) { it.toByte() }
        val rs = LongArray(2 + transport.size)
        runBlocking {
            for (i in 0 until total) {
                val k0 = System.nanoTime()
                val encoded = if (generated) encodeBytes(payload) else payload
                val k1 = System.nanoTime()
                val reply = IstmoRuntime.call(if (generated) "bench.rust" else "bench.inline", "echo", encoded)
                val k5 = System.nanoTime()
                if (generated) Bincode.readBytes(reply, 0)
                val k6 = System.nanoTime()
                Probe.settle(Probe.ON_RESPOND_ENTER, Probe.ON_RESPOND_EXIT)
                BenchNative.readStamps(rs)
                chain.at("K0", total)[i] = k0
                chain.at("K1", total)[i] = k1
                chain.at("H0", total)[i] = rs[0]
                chain.at("H1", total)[i] = rs[1]
                transport.forEachIndexed { k, name -> chain.at(name, total)[i] = rs[2 + k] }
                chain.at("ORE", total)[i] = Probe.read(Probe.ON_RESPOND_ENTER)
                chain.at("ORX", total)[i] = Probe.read(Probe.ON_RESPOND_EXIT)
                chain.at("K5", total)[i] = k5
                chain.at("K6", total)[i] = k6
            }
        }
        val head = listOf(
            Triple("encode (Kotlin Bincode)", "K0", "K1"),
            Triple("JNI Kotlin→Rust nativeCall entry", "K1", "SCE"),
        )
        val middle = if (generated) {
            listOf(
                Triple("strings+byte[] copy, route, decode (inline host)", "SCE", "H0"),
                Triple("host body", "H0", "H1"),
                Triple("reply encode + direct answer", "H1", "SCD"),
            )
        } else {
            listOf(Triple("strings+byte[] copy, inline host, direct answer", "SCE", "SCD"))
        }
        val tail = listOf(
            Triple("reply byte[] + return to Kotlin", "SCD", "K5"),
            Triple("decode (Kotlin Bincode)", "K5", "K6"),
        )
        renderChain(chain, size, head + middle + tail, emptyList())
    }
}
