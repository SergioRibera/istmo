package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Minimal bincode 2 (`standard()` config) codec for the shapes the demo plugin
 * surface needs.
 *
 * Wire format (from `bincode` crate, `VarintEncoding` + `LittleEndian`):
 * * u64 varint — 1 byte for values ≤ 250, otherwise tag byte
 *   (0xFB/0xFC/0xFD) followed by LE u16/u32/u64.
 * * i64 varint — zigzag encoded then u64 varint.
 * * bool — single byte (0x00 / 0x01).
 * * String — u64 varint length + UTF-8 bytes.
 * * Option<T> — 0x00 for None; 0x01 followed by T for Some.
 * * Vec<T> — u64 varint length + T*n.
 * * Tuple `(A, B)` — A followed by B (no length prefix).
 * * enum discriminant — u32 varint.
 */
object Bincode {

    data class Decoded<T>(val value: T, val consumed: Int)

    // ---- Primitives ----

    fun writeBool(out: ByteArrayOutputStream, value: Boolean) {
        out.write(if (value) 1 else 0)
    }

    fun readBool(payload: ByteArray, offset: Int): Decoded<Boolean> {
        require(offset < payload.size) { "bincode bool: buffer underrun" }
        val b = payload[offset].toInt() and 0xFF
        require(b == 0 || b == 1) { "bincode bool: invalid byte 0x${b.toString(16)}" }
        return Decoded(b == 1, offset + 1)
    }

    fun writeString(out: ByteArrayOutputStream, value: String) {
        val utf8 = value.toByteArray(Charsets.UTF_8)
        writeVarintU64(out, utf8.size.toLong())
        out.write(utf8)
    }

    fun writeString(value: String): ByteArray {
        val out = ByteArrayOutputStream()
        writeString(out, value)
        return out.toByteArray()
    }

    /**
     * f32 is written as 4 IEEE 754 little-endian bytes. bincode 2's
     * `standard()` config uses fixed-width encoding for floats — no
     * varint applies to floating-point numbers.
     */
    fun writeF32(out: ByteArrayOutputStream, value: Float) {
        val bits = java.lang.Float.floatToRawIntBits(value)
        for (i in 0 until 4) {
            out.write((bits shr (i * 8)) and 0xFF)
        }
    }

    fun readF32(payload: ByteArray, offset: Int): Decoded<Float> {
        require(offset + 4 <= payload.size) { "bincode f32: buffer underrun" }
        var bits = 0
        for (i in 0 until 4) {
            bits = bits or ((payload[offset + i].toInt() and 0xFF) shl (i * 8))
        }
        return Decoded(java.lang.Float.intBitsToFloat(bits), offset + 4)
    }

    fun readString(payload: ByteArray, offset: Int = 0): Decoded<String> {
        val (length, next) = readVarintU64(payload, offset)
        val end = next + length.toInt()
        require(end <= payload.size) {
            "bincode string overruns buffer: len=$length at offset=$next of ${payload.size}"
        }
        val value = String(payload, next, length.toInt(), Charsets.UTF_8)
        return Decoded(value, end)
    }

    // ---- Combinators ----

    fun <T> writeOption(out: ByteArrayOutputStream, value: T?, writer: (ByteArrayOutputStream, T) -> Unit) {
        if (value == null) {
            out.write(0)
        } else {
            out.write(1)
            writer(out, value)
        }
    }

    fun <T> readOption(payload: ByteArray, offset: Int, reader: (ByteArray, Int) -> Decoded<T>): Decoded<T?> {
        require(offset < payload.size) { "bincode option: buffer underrun" }
        val tag = payload[offset].toInt() and 0xFF
        return when (tag) {
            0 -> Decoded(null, offset + 1)
            1 -> {
                val inner = reader(payload, offset + 1)
                Decoded(inner.value, inner.consumed)
            }
            else -> error("bincode option: invalid tag 0x${tag.toString(16)}")
        }
    }

    fun <T> writeVec(out: ByteArrayOutputStream, items: List<T>, writer: (ByteArrayOutputStream, T) -> Unit) {
        writeVarintU64(out, items.size.toLong())
        for (item in items) writer(out, item)
    }

    fun <T> readVec(payload: ByteArray, offset: Int, reader: (ByteArray, Int) -> Decoded<T>): Decoded<List<T>> {
        val (length, first) = readVarintU64(payload, offset)
        var cursor = first
        val out = ArrayList<T>(length.toInt())
        repeat(length.toInt()) {
            val item = reader(payload, cursor)
            out.add(item.value)
            cursor = item.consumed
        }
        return Decoded(out, cursor)
    }

    fun <A, B> writePair(
        out: ByteArrayOutputStream,
        value: Pair<A, B>,
        writeA: (ByteArrayOutputStream, A) -> Unit,
        writeB: (ByteArrayOutputStream, B) -> Unit,
    ) {
        writeA(out, value.first)
        writeB(out, value.second)
    }

    fun <A, B> readPair(
        payload: ByteArray,
        offset: Int,
        readA: (ByteArray, Int) -> Decoded<A>,
        readB: (ByteArray, Int) -> Decoded<B>,
    ): Decoded<Pair<A, B>> {
        val a = readA(payload, offset)
        val b = readB(payload, a.consumed)
        return Decoded(a.value to b.value, b.consumed)
    }

    // ---- Enums (discriminant only helpers) ----

    /** Write a fieldless enum variant: just the u32-varint discriminant. */
    fun writeEnumDiscriminant(out: ByteArrayOutputStream, discriminant: Int) {
        require(discriminant >= 0) { "enum discriminant must be non-negative" }
        writeVarintU64(out, discriminant.toLong())
    }

    fun readEnumDiscriminant(payload: ByteArray, offset: Int): Decoded<Int> {
        val (value, next) = readVarintU64(payload, offset)
        return Decoded(value.toInt(), next)
    }

    // ---- Varints ----

    fun writeVarintU64(out: ByteArrayOutputStream, value: Long) {
        when {
            value < 0 -> error("bincode varint u64 is unsigned; got $value")
            value <= 250 -> out.write(value.toInt())
            value <= 0xFFFF -> {
                out.write(0xFB)
                writeLittleEndian(out, value, 2)
            }
            value <= 0xFFFF_FFFFL -> {
                out.write(0xFC)
                writeLittleEndian(out, value, 4)
            }
            else -> {
                out.write(0xFD)
                writeLittleEndian(out, value, 8)
            }
        }
    }

    fun readVarintU64(payload: ByteArray, offset: Int): Pair<Long, Int> {
        require(offset < payload.size) { "bincode varint: buffer underrun" }
        val first = payload[offset].toInt() and 0xFF
        return when {
            first <= 250 -> first.toLong() to (offset + 1)
            first == 0xFB -> readLittleEndian(payload, offset + 1, 2) to (offset + 3)
            first == 0xFC -> readLittleEndian(payload, offset + 1, 4) to (offset + 5)
            first == 0xFD -> readLittleEndian(payload, offset + 1, 8) to (offset + 9)
            else -> error("invalid bincode varint prefix: 0x${first.toString(16)}")
        }
    }

    fun writeVarintI64(out: ByteArrayOutputStream, value: Long) {
        // Zigzag encoding then u64 varint.
        val zig = (value shl 1) xor (value shr 63)
        writeVarintU64(out, zig)
    }

    fun readVarintI64(payload: ByteArray, offset: Int): Pair<Long, Int> {
        val (zig, next) = readVarintU64(payload, offset)
        val value = (zig ushr 1) xor -(zig and 1L)
        return value to next
    }

    private fun writeLittleEndian(out: ByteArrayOutputStream, value: Long, bytes: Int) {
        for (i in 0 until bytes) {
            out.write(((value shr (i * 8)) and 0xFF).toInt())
        }
    }

    private fun readLittleEndian(payload: ByteArray, offset: Int, bytes: Int): Long {
        var value = 0L
        for (i in 0 until bytes) {
            value = value or ((payload[offset + i].toLong() and 0xFF) shl (i * 8))
        }
        return value
    }
}
