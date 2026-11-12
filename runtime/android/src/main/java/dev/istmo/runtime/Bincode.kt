package dev.istmo.runtime

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

object Bincode {

    data class Decoded<T>(val value: T, val consumed: Int)

    /// Thrown when a payload does not decode: truncated input, an invalid
    /// tag or a value outside the target type's range. Extends
    /// `IllegalArgumentException` so callers catching the previous
    /// `require` failures keep working.
    class DecodeException(message: String) : IllegalArgumentException(message)

    private inline fun decodeCheck(value: Boolean, message: () -> String) {
        if (!value) throw DecodeException(message())
    }

    fun writeBool(out: ByteArrayOutputStream, value: Boolean) {
        out.write(if (value) 1 else 0)
    }

    fun readBool(payload: ByteArray, offset: Int): Decoded<Boolean> {
        decodeCheck(offset < payload.size) { "bincode bool: buffer underrun" }
        val b = payload[offset].toInt() and 0xFF
        decodeCheck(b == 0 || b == 1) { "bincode bool: invalid byte 0x${b.toString(16)}" }
        return Decoded(b == 1, offset + 1)
    }

    // `u8` / `i8` travel as one raw byte; every wider integer is a varint.

    fun writeU8(out: ByteArrayOutputStream, value: UByte) {
        out.write(value.toInt())
    }

    fun readU8(payload: ByteArray, offset: Int): Decoded<UByte> {
        decodeCheck(offset < payload.size) { "bincode u8: buffer underrun" }
        return Decoded(payload[offset].toUByte(), offset + 1)
    }

    fun writeI8(out: ByteArrayOutputStream, value: Byte) {
        out.write(value.toInt() and 0xFF)
    }

    fun readI8(payload: ByteArray, offset: Int): Decoded<Byte> {
        decodeCheck(offset < payload.size) { "bincode i8: buffer underrun" }
        return Decoded(payload[offset], offset + 1)
    }

    fun writeU16(out: ByteArrayOutputStream, value: UShort) {
        writeVarintU64(out, value.toLong())
    }

    fun readU16(payload: ByteArray, offset: Int): Decoded<UShort> {
        val raw = readVarintU64(payload, offset)
        decodeCheck(raw.value in 0L..0xFFFFL) { "bincode u16: ${raw.value.toULong()} out of range" }
        return Decoded(raw.value.toUShort(), raw.consumed)
    }

    fun writeI16(out: ByteArrayOutputStream, value: Short) {
        writeVarintI64(out, value.toLong())
    }

    fun readI16(payload: ByteArray, offset: Int): Decoded<Short> {
        val raw = readVarintI64(payload, offset)
        decodeCheck(raw.value in Short.MIN_VALUE.toLong()..Short.MAX_VALUE.toLong()) {
            "bincode i16: ${raw.value} out of range"
        }
        return Decoded(raw.value.toShort(), raw.consumed)
    }

    fun writeU32(out: ByteArrayOutputStream, value: UInt) {
        writeVarintU64(out, value.toLong())
    }

    fun readU32(payload: ByteArray, offset: Int): Decoded<UInt> {
        val raw = readVarintU64(payload, offset)
        decodeCheck(raw.value in 0L..0xFFFF_FFFFL) { "bincode u32: ${raw.value.toULong()} out of range" }
        return Decoded(raw.value.toUInt(), raw.consumed)
    }

    fun writeI32(out: ByteArrayOutputStream, value: Int) {
        writeVarintI64(out, value.toLong())
    }

    fun readI32(payload: ByteArray, offset: Int): Decoded<Int> {
        val raw = readVarintI64(payload, offset)
        decodeCheck(raw.value in Int.MIN_VALUE.toLong()..Int.MAX_VALUE.toLong()) {
            "bincode i32: ${raw.value} out of range"
        }
        return Decoded(raw.value.toInt(), raw.consumed)
    }

    fun writeU64(out: ByteArrayOutputStream, value: ULong) {
        writeVarintUnsigned(out, value.toLong())
    }

    fun readU64(payload: ByteArray, offset: Int): Decoded<ULong> {
        val raw = readVarintU64(payload, offset)
        return Decoded(raw.value.toULong(), raw.consumed)
    }

    fun writeI64(out: ByteArrayOutputStream, value: Long) {
        writeVarintI64(out, value)
    }

    fun readI64(payload: ByteArray, offset: Int): Decoded<Long> = readVarintI64(payload, offset)

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

    fun writeF32(out: ByteArrayOutputStream, value: Float) {
        val bits = java.lang.Float.floatToRawIntBits(value)
        for (i in 0 until 4) {
            out.write((bits shr (i * 8)) and 0xFF)
        }
    }

    fun readF32(payload: ByteArray, offset: Int): Decoded<Float> {
        decodeCheck(offset + 4 <= payload.size) { "bincode f32: buffer underrun" }
        var bits = 0
        for (i in 0 until 4) {
            bits = bits or ((payload[offset + i].toInt() and 0xFF) shl (i * 8))
        }
        return Decoded(java.lang.Float.intBitsToFloat(bits), offset + 4)
    }

    fun writeF64(out: ByteArrayOutputStream, value: Double) {
        val bits = java.lang.Double.doubleToRawLongBits(value)
        for (i in 0 until 8) {
            out.write(((bits shr (i * 8)) and 0xFF).toInt())
        }
    }

    fun readF64(payload: ByteArray, offset: Int): Decoded<Double> {
        decodeCheck(offset + 8 <= payload.size) { "bincode f64: buffer underrun" }
        var bits = 0L
        for (i in 0 until 8) {
            bits = bits or ((payload[offset + i].toLong() and 0xFF) shl (i * 8))
        }
        return Decoded(java.lang.Double.longBitsToDouble(bits), offset + 8)
    }

    fun writeBytes(out: ByteArrayOutputStream, value: ByteArray) {
        writeVarintU64(out, value.size.toLong())
        out.write(value)
    }

    fun readBytes(payload: ByteArray, offset: Int): Decoded<ByteArray> {
        val (next, length) = readLength(payload, offset, "bytes")
        val end = next + length
        return Decoded(payload.copyOfRange(next, end), end)
    }

    fun readString(payload: ByteArray, offset: Int = 0): Decoded<String> {
        val (next, length) = readLength(payload, offset, "string")
        // Rust refuses malformed UTF-8; `String(bytes, UTF_8)` would
        // silently substitute U+FFFD and re-encode to different bytes.
        val decoder = Charsets.UTF_8.newDecoder()
            .onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
        val text = try {
            decoder.decode(ByteBuffer.wrap(payload, next, length)).toString()
        } catch (_: CharacterCodingException) {
            throw DecodeException("bincode string: invalid UTF-8 at offset=$next")
        }
        return Decoded(text, next + length)
    }

    /// Reads a byte-length prefix and checks the claimed bytes are present.
    /// Returns `(firstByte, length)`.
    private fun readLength(payload: ByteArray, offset: Int, what: String): Pair<Int, Int> {
        val (length, next) = readVarintU64(payload, offset)
        decodeCheck(length >= 0 && length <= payload.size - next) {
            "bincode $what overruns buffer: len=${length.toULong()} at offset=$next of ${payload.size}"
        }
        return next to length.toInt()
    }

    fun <T> writeOption(out: ByteArrayOutputStream, value: T?, writer: (ByteArrayOutputStream, T) -> Unit) {
        if (value == null) {
            out.write(0)
        } else {
            out.write(1)
            writer(out, value)
        }
    }

    fun <T> readOption(payload: ByteArray, offset: Int, reader: (ByteArray, Int) -> Decoded<T>): Decoded<T?> {
        decodeCheck(offset < payload.size) { "bincode option: buffer underrun" }
        val tag = payload[offset].toInt() and 0xFF
        return when (tag) {
            0 -> Decoded(null, offset + 1)
            1 -> {
                val inner = reader(payload, offset + 1)
                Decoded(inner.value, inner.consumed)
            }
            else -> throw DecodeException("bincode option: invalid tag 0x${tag.toString(16)}")
        }
    }

    fun <T> writeVec(out: ByteArrayOutputStream, items: List<T>, writer: (ByteArrayOutputStream, T) -> Unit) {
        writeVarintU64(out, items.size.toLong())
        for (item in items) writer(out, item)
    }

    fun <T> readVec(payload: ByteArray, offset: Int, reader: (ByteArray, Int) -> Decoded<T>): Decoded<List<T>> {
        val (length, first) = readVarintU64(payload, offset)
        decodeCheck(length >= 0 && length <= Int.MAX_VALUE) {
            "bincode vec: length ${length.toULong()} out of range"
        }
        var cursor = first
        // The claimed length is untrusted: never preallocate past what the
        // remaining bytes could possibly hold.
        val out = ArrayList<T>(minOf(length.toInt(), payload.size - first))
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

    fun writeEnumDiscriminant(out: ByteArrayOutputStream, discriminant: Int) {
        require(discriminant >= 0) { "enum discriminant must be non-negative" }
        writeVarintU64(out, discriminant.toLong())
    }

    fun readEnumDiscriminant(payload: ByteArray, offset: Int): Decoded<Int> {
        val v = readVarintU64(payload, offset)
        decodeCheck(v.value in 0L..Int.MAX_VALUE.toLong()) {
            "bincode enum discriminant: ${v.value.toULong()} out of range"
        }
        return Decoded(v.value.toInt(), v.consumed)
    }

    fun writeVarintU64(out: ByteArrayOutputStream, value: Long) {
        if (value < 0) error("bincode varint u64 is unsigned; got $value (use writeU64 for ULong)")
        writeVarintUnsigned(out, value)
    }

    /// Writes `value` as a varint, reading its 64 bits as unsigned.
    private fun writeVarintUnsigned(out: ByteArrayOutputStream, value: Long) {
        when {
            java.lang.Long.compareUnsigned(value, 250) <= 0 -> out.write(value.toInt())
            java.lang.Long.compareUnsigned(value, 0xFFFF) <= 0 -> {
                out.write(0xFB)
                writeLittleEndian(out, value, 2)
            }
            java.lang.Long.compareUnsigned(value, 0xFFFF_FFFFL) <= 0 -> {
                out.write(0xFC)
                writeLittleEndian(out, value, 4)
            }
            else -> {
                out.write(0xFD)
                writeLittleEndian(out, value, 8)
            }
        }
    }

    /// Returns the varint's 64 bits in a `Long`; values above
    /// `Long.MAX_VALUE` come back negative (see [readU64]).
    fun readVarintU64(payload: ByteArray, offset: Int): Decoded<Long> {
        decodeCheck(offset < payload.size) { "bincode varint: buffer underrun" }
        val first = payload[offset].toInt() and 0xFF
        val width = when {
            first <= 250 -> return Decoded(first.toLong(), offset + 1)
            first == 0xFB -> 2
            first == 0xFC -> 4
            first == 0xFD -> 8
            else -> throw DecodeException("invalid bincode varint prefix: 0x${first.toString(16)}")
        }
        decodeCheck(offset + 1 + width <= payload.size) { "bincode varint: buffer underrun" }
        return Decoded(readLittleEndian(payload, offset + 1, width), offset + 1 + width)
    }

    fun writeVarintI64(out: ByteArrayOutputStream, value: Long) {
        val zig = (value shl 1) xor (value shr 63)
        writeVarintUnsigned(out, zig)
    }

    fun readVarintI64(payload: ByteArray, offset: Int): Decoded<Long> {
        val zig = readVarintU64(payload, offset)
        val value = (zig.value ushr 1) xor -(zig.value and 1L)
        return Decoded(value, zig.consumed)
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
