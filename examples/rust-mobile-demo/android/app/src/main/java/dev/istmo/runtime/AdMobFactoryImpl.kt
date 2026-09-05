package dev.istmo.runtime

import android.app.Activity
import android.util.Log
import com.google.android.gms.ads.MobileAds
import com.google.android.gms.ads.RequestConfiguration
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Instance factory for the `AdMob` plugin. `AdMobDispatcher.handleCreateInstance`
 * decodes the `AdMobConfig` payload the caller shipped through
 * `AdMobClient::acquire_with`, then calls this factory to allocate the
 * per-instance backend.
 *
 * `MobileAds.initialize` runs exactly once per process — the flag on
 * this factory guards subsequent instantiations from re-initialising.
 * Test device ids + child-directed treatment are applied on every call
 * because the caller's config may differ per instance.
 */
class AdMobFactoryImpl(private val activity: Activity) : AdMobFactory {

    companion object {
        private const val TAG = "istmo.admob"
    }

    @Volatile
    private var initialised = false

    override suspend fun create(config: AdMobConfig): AdMobBackend {
        applyConfiguration(config)
        return AdMobBackendImpl(activity)
    }

    private suspend fun applyConfiguration(config: AdMobConfig) {
        withContext(Dispatchers.Main) {
            val builder = RequestConfiguration.Builder()
                .setTestDeviceIds(config.testDeviceIds)
            if (config.childDirectedTreatment) {
                builder.setTagForChildDirectedTreatment(
                    RequestConfiguration.TAG_FOR_CHILD_DIRECTED_TREATMENT_TRUE,
                )
            }
            MobileAds.setRequestConfiguration(builder.build())
            if (!initialised) {
                MobileAds.initialize(activity) { status ->
                    Log.i(TAG, "MobileAds initialised: ${status.adapterStatusMap}")
                }
                initialised = true
            }
        }
    }
}
