package dev.istmo.sharedemo

import android.os.Bundle
import com.google.androidgamesdk.GameActivity
import dev.istmo.plugins.share.ShareBackendImpl
import dev.istmo.runtime.IstmoRuntime
import dev.istmo.runtime.ShareCodecsImpl
import dev.istmo.runtime.ShareDispatcher

/**
 * `GameActivity` is an `AppCompatActivity`, i.e. a `ComponentActivity`,
 * so `ShareBackendImpl` can register its chooser result launcher. The
 * dispatcher is registered before `super.onCreate`, which boots the Rust
 * side (`android_main`) that acquires `ShareClient` right away.
 *
 * `IstmoRuntime.start` also flushes shares that `ShareReceiverActivity`
 * published before the native library was loaded (cold start from
 * another app's share sheet); Rust reads them from `ShareInbox`.
 */
class ShareDemoActivity : GameActivity() {

    override fun onCreate(savedInstanceState: Bundle?) {
        check(IstmoRuntime.start("share_demo")) { "IstmoRuntime.start() failed" }
        IstmoRuntime.registerHandler(
            ShareDispatcher.PLUGIN_ID,
            ShareDispatcher(ShareBackendImpl(this), ShareCodecsImpl()),
        )
        super.onCreate(savedInstanceState)
    }

    override fun onDestroy() {
        super.onDestroy()
        IstmoRuntime.shutdown()
    }
}
