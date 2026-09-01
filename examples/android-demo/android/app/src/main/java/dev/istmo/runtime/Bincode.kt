package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Minimal bincode 2 (`standard()` config) codec for the primitives the demo
 * plugin needs. Extended per plugin as new methods appear.
 *
 * Wire format (from `bincode` crate, `VarintEncoding` + `LittleEndian`):
 * * u64 varint — 1 byte for values ≤ 250, otherwise tag byte
 *   (0xFB/0xFC/0xFD) followed by LE u16/u32/u64.
 * * String — u64 varint length + UTF-8 bytes.
 * * Tuple `(T,)` — encoded as `T` (no length prefix).
 */
object Bincode {

    fun writeString(value: String): ByteArray {
        val utf8 = value.toByteArray(Charsets.UTF_8)
        val out = ByteArrayOutputStream(utf8.size + 5)
        writeVarintU64(out, utf8.size.toLong())
        out.write(utf8)
        return out.toByteArray()
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

    data class Decoded<T>(val value: T, val consumed: Int)

    private fun writeVarintU64(out: ByteArrayOutputStream, value: Long) {
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

    private fun writeLittleEndian(out: ByteArrayOutputStream, value: Long, bytes: Int) {
        for (i in 0 until bytes) {
            out.write(((value shr (i * 8)) and 0xFF).toInt())
        }
    }

    private fun readVarintU64(payload: ByteArray, offset: Int): Pair<Long, Int> {
        val first = payload[offset].toInt() and 0xFF
        return when {
            first <= 250 -> first.toLong() to (offset + 1)
            first == 0xFB -> readLittleEndian(payload, offset + 1, 2) to (offset + 3)
            first == 0xFC -> readLittleEndian(payload, offset + 1, 4) to (offset + 5)
            first == 0xFD -> readLittleEndian(payload, offset + 1, 8) to (offset + 9)
            else -> error("invalid bincode varint prefix: 0x${first.toString(16)}")
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
