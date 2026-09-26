package dev.istmo.biometricdemo

import dev.istmo.runtime.IstmoGameActivity

/**
 * Starts the runtime and registers every plugin before the Rust entry
 * point runs. `GameActivity` is an `AppCompatActivity`, i.e. the
 * `FragmentActivity` that the generated registry hands to
 * `BiometricBackendImpl` for `BiometricPrompt`.
 */
class BiometricDemoActivity : IstmoGameActivity()
