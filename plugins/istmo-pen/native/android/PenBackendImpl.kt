package dev.istmo.plugins.pen

import dev.istmo.runtime.PenBackend
import dev.istmo.runtime.PenCapabilities
import dev.istmo.runtime.PenEvent
import dev.istmo.runtime.PenHoverEvent
import kotlinx.coroutines.flow.Flow

/**
 * Reference [PenBackend] implementation for Android.
 *
 * One instance per window / drawing surface. The [PenCaptureView]
 * provides the actual stylus event source; this wrapper adapts the
 * view's [kotlinx.coroutines.flow.Flow]s to the plugin interface and
 * fills the capability descriptor.
 *
 * The `windowId` is echoed back from [dev.istmo.runtime.PenConfig] so
 * callers can log or route by it; it is not used to select the view —
 * factories map that.
 */
class PenBackendImpl(
    private val view: PenCaptureView,
    @Suppress("unused") private val windowId: ULong,
) : PenBackend {

    override fun events(): Flow<PenEvent> = view.events

    override fun hover(): Flow<PenHoverEvent> = view.hover

    override suspend fun capabilities(): PenCapabilities = view.capabilities

    override suspend fun set_prediction_enabled(enabled: Boolean) {
        // Android's `MotionEvent` surface does not expose a first-party
        // sample predictor; predicted samples stay empty regardless of
        // this setting. Consumers wanting prediction should smooth
        // client-side.
    }
}
