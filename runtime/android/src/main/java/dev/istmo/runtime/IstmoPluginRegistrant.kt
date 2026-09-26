package dev.istmo.runtime

import android.content.Context

/**
 * Implemented by the `dev.istmo.generated.IstmoPluginRegistry` object that
 * `istmo-build` generates into every app: registers each auto-registered
 * plugin dispatcher with [IstmoRuntime].
 *
 * [context] is the host activity when called from [IstmoHost]; plugins
 * whose backend needs an activity are skipped when it is not one (for
 * example when registering from a `Service`).
 */
interface IstmoPluginRegistrant {
    fun registerAll(context: Context)
}
