package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PluginException
import java.io.ByteArrayOutputStream

/**
 * Hand-written Kotlin client for the Rust-hosted `Echo` plugin.
 *
 * This is what a `istmo-build` code generator would emit for a
 * `hosts:` entry — a suspend fun per trait method that bincode-encodes
 * arguments, ships them across the frame protocol via
 * [IstmoRuntime.call], and decodes the response.
 *
 * TODO: promote this file to codegen in a follow-up.
 */
object EchoClient {

    private const val PLUGIN_ID = "dev.istmo.demo.echo"

    /** `Echo::echo(text) -> Result<String, EchoError>` on the Rust side. */
    suspend fun echo(text: String): String {
        val payload = ByteArrayOutputStream().apply { Bincode.writeString(this, text) }.toByteArray()
        val response = try {
            IstmoRuntime.call(PLUGIN_ID, "echo", payload)
        } catch (e: PluginException) {
            val (reason, _) = decodeEchoError(e.payload)
            throw EchoException(reason)
        }
        return Bincode.readString(response).value
    }

    private fun decodeEchoError(bytes: ByteArray): Bincode.Decoded<String> {
        // EchoError is a struct with a single `reason: String` field.
        return Bincode.readString(bytes)
    }
}

class EchoException(val reason: String) : RuntimeException(reason)
