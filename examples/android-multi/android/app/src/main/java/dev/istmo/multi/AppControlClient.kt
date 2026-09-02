package dev.istmo.multi

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.PluginException

/**
 * Hand-written Kotlin client for the Rust-hosted `AppControl` trait.
 * Will be codegen'd from `istmo-build` in a follow-up; for now the demo
 * writes it by hand to stay self-contained.
 */
object AppControlClient {
    private const val PLUGIN_ID = "dev.istmo.multi.app_control"

    suspend fun ping(): String {
        val bytes = try {
            IstmoRuntime.call(PLUGIN_ID, "ping", ByteArray(0))
        } catch (e: PluginException) {
            throw AppException(decodeDomainError(e.payload))
        }
        val (value, _) = Bincode.readString(bytes)
        return value
    }

    private fun decodeDomainError(payload: ByteArray): String {
        val (reason, _) = Bincode.readString(payload, 0)
        return reason
    }
}

class AppException(val reason: String) : RuntimeException(reason)
