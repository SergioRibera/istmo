package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PluginException
import java.io.ByteArrayOutputStream

/**
 * Hand-written Kotlin client for the Rust-hosted `Echo` plugin.
 *
 * This is what a Kotlin code generator would emit for a `hosts:` entry —
 * a suspend fun per trait method that bincode-encodes arguments, ships
 * them across the frame protocol via [IstmoRuntime.call], and decodes the
 * response. All `Echo` methods return `Result<T, EchoError>` on the Rust
 * side; the domain error surfaces here as [EchoException].
 *
 * TODO: promote this file to codegen in a follow-up (`istmo-build` should
 * emit it from `Contract` metadata).
 */
object EchoClient {

    private const val PLUGIN_ID = "dev.istmo.demo.echo"
    private val EMPTY_PAYLOAD = ByteArray(0)

    suspend fun echo(text: String): String =
        callString("echo", encodeString(text))

    suspend fun checkPermission(permission: String): String =
        callString("check_permission", encodeString(permission))

    suspend fun requestPermission(permission: String): String =
        callString("request_permission", encodeString(permission))

    suspend fun openUrl(url: String): String =
        callString("open_url", encodeString(url))

    suspend fun lifecycleSnapshot(): String =
        callString("lifecycle_snapshot", EMPTY_PAYLOAD)

    suspend fun drainDeeplinks(): List<String> {
        val response = callRaw("drain_deeplinks", EMPTY_PAYLOAD)
        return Bincode.readVec(response, 0, Bincode::readString).value
    }

    // ---- Internals ----

    private suspend fun callString(method: String, payload: ByteArray): String {
        val response = callRaw(method, payload)
        return Bincode.readString(response).value
    }

    private suspend fun callRaw(method: String, payload: ByteArray): ByteArray = try {
        IstmoRuntime.call(PLUGIN_ID, method, payload)
    } catch (e: PluginException) {
        // EchoError is a struct with a single `reason: String` field.
        val (reason, _) = Bincode.readString(e.payload)
        throw EchoException(reason)
    }

    private fun encodeString(value: String): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeString(out, value)
        return out.toByteArray()
    }
}

class EchoException(val reason: String) : RuntimeException(reason)
