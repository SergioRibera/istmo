package dev.istmo.demo

import dev.istmo.demo.gen.EchoCodecsImpl
import dev.istmo.demo.gen.EchoError
import dev.istmo.demo.gen.NotifierBackend
import dev.istmo.demo.gen.NotifierCodecs
import java.io.ByteArrayOutputStream

class NotifierBackendImpl(private val sink: (String) -> Unit) : NotifierBackend {
    override suspend fun notify(message: String) {
        sink(message)
    }
}

class NotifierCodecsAdapter(
    private val echoCodecs: EchoCodecsImpl = EchoCodecsImpl(),
) : NotifierCodecs {
    override fun readEchoError(bytes: ByteArray, offset: Int) =
        echoCodecs.readEchoError(bytes, offset)

    override fun writeEchoError(out: ByteArrayOutputStream, value: EchoError) =
        echoCodecs.writeEchoError(out, value)
}

