// Decodes every value of `crates/build/tests/golden/coverage_wire.hex.expected`
// (what Rust sends) with the generated `coverage` codecs, writes it back and
// checks the bytes are the same. A `<Type>+` line is a variant a later
// version added, which must decode as `Unknown`. Run by `compile-goldens.sh`.
package dev.istmo.runtime

import java.io.ByteArrayOutputStream
import java.io.File
import kotlin.system.exitProcess

private fun unhex(text: String): ByteArray =
    ByteArray(text.length / 2) { i -> text.substring(2 * i, 2 * i + 2).toInt(16).toByte() }

private fun <T> roundTrip(
    bytes: ByteArray,
    read: (ByteArray, Int) -> Bincode.Decoded<T>,
    write: (ByteArrayOutputStream, T) -> Unit,
): T {
    val decoded = read(bytes, 0)
    check(decoded.consumed == bytes.size) { "read ${decoded.consumed} of ${bytes.size} bytes" }
    val out = ByteArrayOutputStream()
    write(out, decoded.value)
    check(out.toByteArray().contentEquals(bytes)) { "written back as different bytes" }
    return decoded.value
}

fun main(args: Array<String>) {
    val codecs = CoverageCodecsImpl()
    var failures = 0
    for (line in File(args[0]).readLines().filter { it.isNotBlank() }) {
        val (name, hex) = line.split(' ', limit = 2)
        val bytes = unhex(hex)
        try {
            val value: Any? = when (name.removeSuffix("+")) {
                "Everything" -> roundTrip(bytes, codecs::readEverything, codecs::writeEverything)
                "Shape" -> roundTrip(bytes, codecs::readShape, codecs::writeShape)
                "Mode" -> roundTrip(bytes, codecs::readMode, codecs::writeMode)
                "FirstError" -> roundTrip(bytes, codecs::readFirstError, codecs::writeFirstError)
                "SecondError" -> roundTrip(bytes, codecs::readSecondError, codecs::writeSecondError)
                "Level" -> roundTrip(bytes, codecs::readLevel, codecs::writeLevel)
                "Signal" -> roundTrip(bytes, codecs::readSignal, codecs::writeSignal)
                else -> error("no codec for $name")
            }
            if (name.endsWith("+")) {
                check(value is Level.Unknown || value is Signal.Unknown) { "decoded as $value" }
            }
        } catch (e: IllegalStateException) {
            System.err.println("$name $hex: ${e.message}")
            failures += 1
        } catch (e: Bincode.DecodeException) {
            System.err.println("$name $hex: ${e.message}")
            failures += 1
        }
    }
    if (failures > 0) exitProcess(1)
    println("coverage wire values round-trip through the generated Kotlin codecs")
}
