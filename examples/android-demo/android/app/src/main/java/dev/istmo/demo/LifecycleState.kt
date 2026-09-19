package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import java.io.ByteArrayOutputStream

enum class LifecycleState {
    Created,
    Started,
    Resumed,
    Paused,
    Stopped,
    Destroyed,
    LowMemory,
    ConfigurationChanged;

    fun encode(): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeEnumDiscriminant(out, ordinal)
        return out.toByteArray()
    }

    fun publish() {
        IstmoRuntime.submitEarlyLatest(LIFECYCLE_CHANNEL, encode())
    }

    companion object {
        const val LIFECYCLE_CHANNEL: String = "istmo.lifecycle"
    }
}

