package dev.istmo.camerademo

import dev.istmo.runtime.IstmoGameActivity

/**
 * One-liner activity. `IstmoGameActivity.onCreate` boots the runtime
 * and runs `IstmoPluginRegistry.registerAll(this, runtime)` — the
 * generated registry instantiates `CameraFactoryImpl(this)` with this
 * `ComponentActivity`, satisfying the camera plugin's
 * `android_backend_arg = "activity"` wiring.
 */
class CameraDemoActivity : IstmoGameActivity()
