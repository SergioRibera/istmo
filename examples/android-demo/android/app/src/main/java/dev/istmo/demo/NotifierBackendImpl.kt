package dev.istmo.demo

import dev.istmo.demo.gen.EchoCodecsImpl
import dev.istmo.demo.gen.EchoError
import dev.istmo.demo.gen.NotifierBackend
import dev.istmo.demo.gen.NotifierCodecs
import java.io.ByteArrayOutputStream

/**
 * Pure-Kotlin backend for the `dev.istmo.demo.notifier` plugin. The
 * generated `NotifierDispatcher` handles wire decode + encode; this
 * class only forwards decoded messages to the UI sink.
 */
class NotifierBackendImpl(private val sink: (String) -> Unit) : NotifierBackend {
    override suspend fun notify(message: String) {
        sink(message)
    }
}

/**
 * Adapter codecs impl: `Notifier`'s wire error type is `EchoError`,
 * declared by the sibling `Echo` contract. Delegating avoids emitting a
 * duplicate `data class EchoError` from `NotifierTypes.kt`.
 */
class NotifierCodecsAdapter(
    private val echoCodecs: EchoCodecsImpl = EchoCodecsImpl(),
) : NotifierCodecs {
    override fun readEchoError(bytes: ByteArray, offset: Int) =
        echoCodecs.readEchoError(bytes, offset)

    override fun writeEchoError(out: ByteArrayOutputStream, value: EchoError) =
        echoCodecs.writeEchoError(out, value)
}
