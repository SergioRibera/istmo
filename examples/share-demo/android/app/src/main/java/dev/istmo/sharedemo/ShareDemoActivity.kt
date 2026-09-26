package dev.istmo.sharedemo

import dev.istmo.runtime.IstmoGameActivity

/**
 * Starts the runtime and registers every plugin (the generated
 * `IstmoPluginRegistry` hands `ShareBackendImpl` this activity) before
 * the Rust entry point runs. `GameActivity` is a `ComponentActivity`, so
 * the share backend can register its chooser result launcher.
 *
 * `IstmoRuntime.start` also flushes shares that `ShareReceiverActivity`
 * published before the native library was loaded (cold start from
 * another app's share sheet); Rust reads them from `ShareInbox`.
 */
class ShareDemoActivity : IstmoGameActivity()
