package dev.istmo.plugins.pen

import dev.istmo.runtime.PenBackend
import dev.istmo.runtime.PenConfig
import dev.istmo.runtime.PenFactory

/**
 * Reference [PenFactory] for single-window apps.
 *
 * Multi-window apps need a per-window [PenCaptureView] and cannot use
 * this factory directly — subclass or write a bespoke factory that
 * routes [PenConfig.windowId] to the matching view. Because the
 * factory requires a `PenCaptureView` reference the app assembles at
 * UI-construction time, `istmo-pen` opts out of auto-registration
 * (`auto_register = false` in its `istmo.toml`) and expects the app
 * to wire the host manually:
 *
 * ```kotlin
 * val view = PenCaptureView(context)
 * IstmoRuntime.registerHandler(PenFactoryImpl(view))
 * ```
 */
class PenFactoryImpl(private val view: PenCaptureView) : PenFactory {
    override suspend fun create(config: PenConfig): PenBackend =
        PenBackendImpl(view, config.windowId)
}
