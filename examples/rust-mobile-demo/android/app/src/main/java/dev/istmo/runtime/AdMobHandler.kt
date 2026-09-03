package dev.istmo.runtime

import android.app.Activity
import android.util.Log
import android.view.Gravity
import android.view.ViewGroup
import android.widget.FrameLayout
import com.google.android.gms.ads.AdError as GmsAdError
import com.google.android.gms.ads.AdListener
import com.google.android.gms.ads.AdRequest
import com.google.android.gms.ads.AdSize
import com.google.android.gms.ads.AdView
import com.google.android.gms.ads.FullScreenContentCallback
import com.google.android.gms.ads.LoadAdError
import com.google.android.gms.ads.MobileAds
import com.google.android.gms.ads.RequestConfiguration
import com.google.android.gms.ads.interstitial.InterstitialAd
import com.google.android.gms.ads.interstitial.InterstitialAdLoadCallback
import com.google.android.gms.ads.rewarded.RewardedAd
import com.google.android.gms.ads.rewarded.RewardedAdLoadCallback
import java.io.ByteArrayOutputStream
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicLong
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

/**
 * Kotlin backend for the `istmo.admob` plugin.
 *
 * Every ad object (`InterstitialAd`, `RewardedAd`, `AdView`) is owned
 * natively and referenced from Rust through a `NativeHandleId` allocated
 * via [IstmoRuntime.allocHandleId]. Show / hide / cancel calls consume
 * the handle; dropping the Rust-side `NativeHandle<T>` cascades through
 * [IstmoRuntime.onReleaseNativeHandle] into [releaseNativeHandle] on
 * this handler.
 *
 * Banner overlay: the plugin adds the `AdView` as a child of the
 * activity's content root (`android.R.id.content`) with a
 * `FrameLayout.LayoutParams` sized in physical pixels. Rust picks the
 * rect — the demo uses the egui coordinate space, which matches what
 * `NativeActivity` gives the content view.
 */
class AdMobHandler(private val activity: Activity) : PluginHandler, HandleReleaser {

    companion object {
        const val PLUGIN_ID = "istmo.admob"
        private const val TAG = "istmo.admob"
    }

    private val nextInstanceId = AtomicLong(1)
    private val configs = ConcurrentHashMap<Long, AdMobConfig>()
    private val interstitials = ConcurrentHashMap<Long, InterstitialAd>()
    private val rewardeds = ConcurrentHashMap<Long, RewardedAd>()
    private val banners = ConcurrentHashMap<Long, AdView>()

    @Volatile
    private var initialised = false

    override fun releaseNativeHandle(handleId: Long) {
        interstitials.remove(handleId)
        rewardeds.remove(handleId)
        banners.remove(handleId)?.let { view ->
            activity.runOnUiThread { removeBannerView(view) }
        }
    }

    override suspend fun handleCreateInstance(payload: ByteArray): ByteArray {
        val config = decodeConfig(payload)
        ensureInitialised(config)
        val instanceId = nextInstanceId.getAndIncrement()
        configs[instanceId] = config
        val out = ByteArrayOutputStream(4)
        Bincode.writeVarintU64(out, instanceId)
        return out.toByteArray()
    }

    override suspend fun handleCall(
        instanceId: Long,
        method: String,
        payload: ByteArray,
    ): ByteArray = when (method) {
        "load_interstitial" -> {
            val (unit, _) = Bincode.readString(payload)
            encodeHandleId(loadInterstitial(unit))
        }
        "show_interstitial" -> {
            val (handleId, _) = Bincode.readVarintU64(payload, 0)
            encodeInterstitialOutcome(showInterstitial(handleId))
        }
        "load_rewarded" -> {
            val (unit, _) = Bincode.readString(payload)
            encodeHandleId(loadRewarded(unit))
        }
        "show_rewarded" -> {
            val (handleId, _) = Bincode.readVarintU64(payload, 0)
            encodeRewardedOutcome(showRewarded(handleId))
        }
        "show_banner" -> {
            var cursor = 0
            val (unit, c1) = Bincode.readString(payload, cursor); cursor = c1
            val rect = decodeBannerRect(payload, cursor)
            encodeHandleId(showBanner(unit, rect))
        }
        "update_banner" -> {
            var cursor = 0
            val (handleId, c1) = Bincode.readVarintU64(payload, cursor); cursor = c1
            val rect = decodeBannerRect(payload, cursor)
            updateBanner(handleId, rect)
            ByteArray(0)
        }
        "hide_banner" -> {
            val (handleId, _) = Bincode.readVarintU64(payload, 0)
            hideBanner(handleId)
            ByteArray(0)
        }
        else -> error("unknown AdMob method: $method")
    }

    // ---- SDK init -------------------------------------------------------

    private suspend fun ensureInitialised(config: AdMobConfig) {
        if (initialised) return
        withContext(Dispatchers.Main) {
            if (initialised) return@withContext
            val builder = RequestConfiguration.Builder()
                .setTestDeviceIds(config.testDeviceIds)
            if (config.childDirectedTreatment) {
                builder.setTagForChildDirectedTreatment(
                    RequestConfiguration.TAG_FOR_CHILD_DIRECTED_TREATMENT_TRUE,
                )
            }
            MobileAds.setRequestConfiguration(builder.build())
            MobileAds.initialize(activity) { status ->
                Log.i(TAG, "MobileAds initialised: ${status.adapterStatusMap}")
            }
            initialised = true
        }
    }

    // ---- Interstitial ---------------------------------------------------

    private suspend fun loadInterstitial(adUnitId: String): Long {
        val ad: InterstitialAd = withContext(Dispatchers.Main) {
            suspendCoroutine { cont ->
                InterstitialAd.load(
                    activity,
                    adUnitId,
                    AdRequest.Builder().build(),
                    object : InterstitialAdLoadCallback() {
                        override fun onAdLoaded(ad: InterstitialAd) {
                            cont.resume(ad)
                        }

                        override fun onAdFailedToLoad(err: LoadAdError) {
                            cont.resumeWithException(loadAdException(err))
                        }
                    },
                )
            }
        }
        val handleId = IstmoRuntime.allocHandleId(PLUGIN_ID)
        interstitials[handleId] = ad
        return handleId
    }

    private suspend fun showInterstitial(handleId: Long): InterstitialOutcome {
        val ad = interstitials.remove(handleId) ?: run {
            IstmoRuntime.forgetHandle(handleId)
            throw PluginException(encodeError(Err.UnknownAd, null))
        }
        IstmoRuntime.forgetHandle(handleId)

        return withContext(Dispatchers.Main) {
            suspendCoroutine<InterstitialOutcome> { cont ->
                ad.fullScreenContentCallback = object : FullScreenContentCallback() {
                    override fun onAdDismissedFullScreenContent() {
                        cont.resume(InterstitialOutcome.Dismissed)
                    }

                    override fun onAdFailedToShowFullScreenContent(err: GmsAdError) {
                        Log.w(TAG, "interstitial failed to show: ${err.code} ${err.message}")
                        cont.resume(InterstitialOutcome.FailedToShow)
                    }
                }
                ad.show(activity)
            }
        }
    }

    // ---- Rewarded -------------------------------------------------------

    private suspend fun loadRewarded(adUnitId: String): Long {
        val ad: RewardedAd = withContext(Dispatchers.Main) {
            suspendCoroutine { cont ->
                RewardedAd.load(
                    activity,
                    adUnitId,
                    AdRequest.Builder().build(),
                    object : RewardedAdLoadCallback() {
                        override fun onAdLoaded(ad: RewardedAd) {
                            cont.resume(ad)
                        }

                        override fun onAdFailedToLoad(err: LoadAdError) {
                            cont.resumeWithException(loadAdException(err))
                        }
                    },
                )
            }
        }
        val handleId = IstmoRuntime.allocHandleId(PLUGIN_ID)
        rewardeds[handleId] = ad
        return handleId
    }

    private suspend fun showRewarded(handleId: Long): RewardedResult {
        val ad = rewardeds.remove(handleId) ?: run {
            IstmoRuntime.forgetHandle(handleId)
            throw PluginException(encodeError(Err.UnknownAd, null))
        }
        IstmoRuntime.forgetHandle(handleId)

        return withContext(Dispatchers.Main) {
            suspendCoroutine<RewardedResult> { cont ->
                var granted: RewardedResult? = null
                ad.fullScreenContentCallback = object : FullScreenContentCallback() {
                    override fun onAdDismissedFullScreenContent() {
                        cont.resume(
                            granted
                                ?: RewardedResult(false, "", 0),
                        )
                    }

                    override fun onAdFailedToShowFullScreenContent(err: GmsAdError) {
                        Log.w(TAG, "rewarded failed to show: ${err.code} ${err.message}")
                        cont.resume(RewardedResult(false, "", 0))
                    }
                }
                ad.show(activity) { reward ->
                    granted = RewardedResult(
                        granted = true,
                        type = reward.type,
                        amount = reward.amount,
                    )
                }
            }
        }
    }

    // ---- Banner ---------------------------------------------------------

    private suspend fun showBanner(adUnitId: String, rect: BannerRectData): Long {
        val handleId = IstmoRuntime.allocHandleId(PLUGIN_ID)
        withContext(Dispatchers.Main) {
            val view = AdView(activity)
            view.adUnitId = adUnitId
            val adSize = pickAdSize(rect)
            view.setAdSize(adSize)
            val paintedSizePx = adSizePixels(adSize)
            Log.i(
                TAG,
                "banner request unit=$adUnitId rectPx=${rect.width}x${rect.height}@${rect.x},${rect.y} " +
                    "-> adSize=${adSize.width}x${adSize.height}dp = ${paintedSizePx.width}x${paintedSizePx.height}px",
            )
            view.adListener = object : AdListener() {
                override fun onAdLoaded() {
                    Log.i(TAG, "banner loaded ($adUnitId)")
                }

                override fun onAdFailedToLoad(err: LoadAdError) {
                    Log.w(
                        TAG,
                        "banner failed to load: code=${err.code} domain=${err.domain} msg=${err.message} " +
                            "cause=${err.cause?.message}",
                    )
                }

                override fun onAdImpression() {
                    Log.i(TAG, "banner impression recorded")
                }
            }
            attachBannerView(view, rect, paintedSizePx)
            view.loadAd(AdRequest.Builder().build())
            banners[handleId] = view
        }
        return handleId
    }

    private suspend fun updateBanner(handleId: Long, rect: BannerRectData) {
        val view = banners[handleId] ?: return
        withContext(Dispatchers.Main) {
            // The SDK-chosen height dominates — reuse the current adSize
            // rather than trusting Rust's rect.height, which is only a
            // hint from the egui layout pass.
            val painted = adSizePixels(view.adSize ?: AdSize.BANNER)
            val params = FrameLayout.LayoutParams(painted.width, painted.height).apply {
                leftMargin = rect.x
                topMargin = rect.y
                gravity = Gravity.TOP or Gravity.START
            }
            view.layoutParams = params
            view.requestLayout()
        }
    }

    private suspend fun hideBanner(handleId: Long) {
        val view = banners.remove(handleId) ?: run {
            IstmoRuntime.forgetHandle(handleId)
            return
        }
        IstmoRuntime.forgetHandle(handleId)
        withContext(Dispatchers.Main) { removeBannerView(view) }
    }

    private fun attachBannerView(view: AdView, rect: BannerRectData, painted: PxSize) {
        val root = activity.findViewById<ViewGroup>(android.R.id.content)
        val params = FrameLayout.LayoutParams(painted.width, painted.height).apply {
            leftMargin = rect.x
            topMargin = rect.y
            gravity = Gravity.TOP or Gravity.START
        }
        root.addView(view, params)
    }

    /**
     * Pick a real AdMob banner size. `AdView.setAdSize(AdSize(w,h))` with
     * arbitrary dimensions is a "custom size" that the standard banner
     * inventory does not fill — even Google's own test banner unit
     * (`.../6300978111`) only fills documented sizes (BANNER, adaptive,
     * MEDIUM_RECTANGLE, …). Rust hands us pixels from the egui layout
     * pass; we convert to dp and ask the SDK for an anchored adaptive
     * banner sized to that width. Falls back to `AdSize.BANNER` if the
     * requested width is too narrow to be adaptive-eligible.
     */
    private fun pickAdSize(rect: BannerRectData): AdSize {
        val density = activity.resources.displayMetrics.density
        val widthDp = (rect.width / density).toInt().coerceAtLeast(0)
        if (widthDp < AdSize.BANNER.width) {
            return AdSize.BANNER
        }
        return AdSize.getCurrentOrientationAnchoredAdaptiveBannerAdSize(activity, widthDp)
    }

    private fun adSizePixels(size: AdSize): PxSize {
        val widthPx = if (size.width > 0) size.getWidthInPixels(activity) else 0
        val heightPx = if (size.height > 0) size.getHeightInPixels(activity) else 0
        return PxSize(widthPx, heightPx)
    }

    private fun removeBannerView(view: AdView) {
        val parent = view.parent as? ViewGroup ?: return
        parent.removeView(view)
        view.destroy()
    }

    // ---- Wire encoding --------------------------------------------------

    /** `AdMobConfig`: String appId + Vec<String> testDeviceIds + bool childDirectedTreatment. */
    private fun decodeConfig(payload: ByteArray): AdMobConfig {
        var cursor = 0
        val (appId, c1) = Bincode.readString(payload, cursor); cursor = c1
        val (ids, c2) = Bincode.readVec(payload, cursor, Bincode::readString); cursor = c2
        val (child, _) = Bincode.readBool(payload, cursor)
        return AdMobConfig(appId, ids, child)
    }

    /** `BannerRect`: four u32 varints. */
    private fun decodeBannerRect(payload: ByteArray, offset: Int): BannerRectData {
        var cursor = offset
        val (x, c1) = Bincode.readVarintU64(payload, cursor); cursor = c1
        val (y, c2) = Bincode.readVarintU64(payload, cursor); cursor = c2
        val (w, c3) = Bincode.readVarintU64(payload, cursor); cursor = c3
        val (h, _) = Bincode.readVarintU64(payload, cursor)
        return BannerRectData(x.toInt(), y.toInt(), w.toInt(), h.toInt())
    }

    private fun encodeHandleId(id: Long): ByteArray {
        val out = ByteArrayOutputStream(4)
        Bincode.writeVarintU64(out, id)
        return out.toByteArray()
    }

    /** `InterstitialOutcome`: enum discriminant. */
    private fun encodeInterstitialOutcome(outcome: InterstitialOutcome): ByteArray {
        val out = ByteArrayOutputStream(1)
        Bincode.writeEnumDiscriminant(out, outcome.ordinal)
        return out.toByteArray()
    }

    /**
     * `RewardedOutcome`: bool granted + String type + u32 amount.
     */
    private fun encodeRewardedOutcome(result: RewardedResult): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeBool(out, result.granted)
        Bincode.writeString(out, result.type)
        Bincode.writeVarintU64(out, result.amount.toLong())
        return out.toByteArray()
    }

    /**
     * `AdError`:
     *   0 NotInitialized
     *   1 NoFill
     *   2 Network(String)
     *   3 InvalidRequest(String)
     *   4 UnknownAd
     *   5 Internal(String)
     */
    private fun encodeError(err: Err, message: String?): ByteArray {
        val out = ByteArrayOutputStream()
        Bincode.writeEnumDiscriminant(out, err.ordinal)
        if (err.hasPayload) {
            Bincode.writeString(out, message ?: "")
        }
        return out.toByteArray()
    }

    /**
     * Map an `AdRequest`-native error code to our typed [Err] variant.
     * Codes are stable across the SDK per Google's documented enum, so
     * we use the numeric constants directly to avoid coupling to a
     * specific SDK class location.
     *   0 = INTERNAL_ERROR, 1 = INVALID_REQUEST,
     *   2 = NETWORK_ERROR,  3 = NO_FILL
     */
    private fun loadAdException(err: LoadAdError): PluginException {
        val (variant, msg) = when (err.code) {
            3 -> Err.NoFill to null
            2 -> Err.Network to err.message
            1 -> Err.InvalidRequest to err.message
            else -> Err.Internal to "code=${err.code} ${err.message}"
        }
        return PluginException(encodeError(variant, msg))
    }

    // ---- Types ----------------------------------------------------------

    private data class AdMobConfig(
        val appId: String,
        val testDeviceIds: List<String>,
        val childDirectedTreatment: Boolean,
    )

    private data class BannerRectData(
        val x: Int,
        val y: Int,
        val width: Int,
        val height: Int,
    )

    private data class PxSize(val width: Int, val height: Int)

    /** Mirrors Rust `istmo::plugins::InterstitialOutcome` variant order. */
    private enum class InterstitialOutcome {
        Dismissed,
        FailedToShow,
    }

    private data class RewardedResult(
        val granted: Boolean,
        val type: String,
        val amount: Int,
    )

    private enum class Err(val hasPayload: Boolean) {
        NotInitialized(false),
        NoFill(false),
        Network(true),
        InvalidRequest(true),
        UnknownAd(false),
        Internal(true),
    }
}

