package dev.istmo.demo

import dev.istmo.runtime.Bincode
import dev.istmo.runtime.IstmoRuntime
import java.io.ByteArrayOutputStream

/**
 * Mirror of the Rust `istmo::plugins::LifecycleState` enum.
 *
 * The wire encoding is a single `u32` varint discriminant (bincode fieldless
 * enum). KEEP THIS ORDER in sync with the Rust enum declaration order.
 */
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

    /** Publish this transition on the `istmo.lifecycle` early-event channel. */
    fun publish() {
        IstmoRuntime.submitEarlyLatest(LIFECYCLE_CHANNEL, encode())
    }

    companion object {
        const val LIFECYCLE_CHANNEL: String = "istmo.lifecycle"
    }
}
