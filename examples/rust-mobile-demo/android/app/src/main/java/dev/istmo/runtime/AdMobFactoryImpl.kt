package dev.istmo.runtime

import android.app.Activity
import android.util.Log
import com.google.android.gms.ads.MobileAds
import com.google.android.gms.ads.RequestConfiguration
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

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

