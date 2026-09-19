package dev.istmo.runtime

import android.app.Activity
import android.util.Log
import android.view.Gravity
import android.view.ViewGroup
import android.widget.FrameLayout
import android.widget.PopupWindow
import com.google.android.gms.ads.AdError as GmsAdError
import com.google.android.gms.ads.AdListener
import com.google.android.gms.ads.AdRequest
import com.google.android.gms.ads.AdSize
import com.google.android.gms.ads.AdView
import com.google.android.gms.ads.FullScreenContentCallback
import com.google.android.gms.ads.LoadAdError
import com.google.android.gms.ads.interstitial.InterstitialAd
import com.google.android.gms.ads.interstitial.InterstitialAdLoadCallback
import com.google.android.gms.ads.rewarded.RewardedAd
import com.google.android.gms.ads.rewarded.RewardedAdLoadCallback
import java.util.concurrent.ConcurrentHashMap
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException
import kotlin.coroutines.suspendCoroutine
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext

class AdMobBackendImpl(private val activity: Activity) : AdMobBackend, HandleReleaser {

    companion object {
        private const val TAG = "istmo.admob"
    }

    private val interstitials = ConcurrentHashMap<NativeHandleId, InterstitialAd>()
    private val rewardeds = ConcurrentHashMap<NativeHandleId, RewardedAd>()

    private data class BannerEntry(val view: AdView, val popup: PopupWindow)
    private val banners = ConcurrentHashMap<NativeHandleId, BannerEntry>()

    override fun releaseNativeHandle(handleId: Long) {
        interstitials.remove(handleId)
        rewardeds.remove(handleId)
        banners.remove(handleId)?.let { entry ->
            activity.runOnUiThread { tearDownBanner(entry) }
        }
    }

    override suspend fun load_interstitial(ad_unit_id: String): NativeHandleId {
        val ad: InterstitialAd = try {
            withContext(Dispatchers.Main) {
                suspendCoroutine { cont ->
                    InterstitialAd.load(
                        activity,
                        ad_unit_id,
                        AdRequest.Builder().build(),
                        object : InterstitialAdLoadCallback() {
                            override fun onAdLoaded(ad: InterstitialAd) {
                                cont.resume(ad)
                            }

                            override fun onAdFailedToLoad(err: LoadAdError) {
                                cont.resumeWithException(BackendException(mapLoadError(err)))
                            }
                        },
                    )
                }
            }
        } catch (e: BackendException) {
            throw e
        }
        val handleId = IstmoRuntime.allocHandleId(AdMobDispatcher.PLUGIN_ID)
        interstitials[handleId] = ad
        return handleId
    }

    override suspend fun show_interstitial(ad: NativeHandleId): InterstitialOutcome {
        val entry = interstitials.remove(ad) ?: run {
            IstmoRuntime.forgetHandle(ad)
            throw BackendException(AdError.UnknownAd)
        }
        IstmoRuntime.forgetHandle(ad)
        return withContext(Dispatchers.Main) {
            suspendCoroutine<InterstitialOutcome> { cont ->
                entry.fullScreenContentCallback = object : FullScreenContentCallback() {
                    override fun onAdDismissedFullScreenContent() {
                        cont.resume(InterstitialOutcome.Dismissed)
                    }

                    override fun onAdFailedToShowFullScreenContent(err: GmsAdError) {
                        Log.w(TAG, "interstitial failed to show: ${err.code} ${err.message}")
                        cont.resume(InterstitialOutcome.FailedToShow)
                    }
                }
                entry.show(activity)
            }
        }
    }

    override suspend fun load_rewarded(ad_unit_id: String): NativeHandleId {
        val ad: RewardedAd = withContext(Dispatchers.Main) {
            suspendCoroutine { cont ->
                RewardedAd.load(
                    activity,
                    ad_unit_id,
                    AdRequest.Builder().build(),
                    object : RewardedAdLoadCallback() {
                        override fun onAdLoaded(ad: RewardedAd) {
                            cont.resume(ad)
                        }

                        override fun onAdFailedToLoad(err: LoadAdError) {
                            cont.resumeWithException(BackendException(mapLoadError(err)))
                        }
                    },
                )
            }
        }
        val handleId = IstmoRuntime.allocHandleId(AdMobDispatcher.PLUGIN_ID)
        rewardeds[handleId] = ad
        return handleId
    }

    override suspend fun show_rewarded(ad: NativeHandleId): RewardedOutcome {
        val entry = rewardeds.remove(ad) ?: run {
            IstmoRuntime.forgetHandle(ad)
            throw BackendException(AdError.UnknownAd)
        }
        IstmoRuntime.forgetHandle(ad)
        return withContext(Dispatchers.Main) {
            suspendCoroutine<RewardedOutcome> { cont ->
                var granted: RewardedOutcome? = null
                entry.fullScreenContentCallback = object : FullScreenContentCallback() {
                    override fun onAdDismissedFullScreenContent() {
                        cont.resume(granted ?: RewardedOutcome(false, "", 0u))
                    }

                    override fun onAdFailedToShowFullScreenContent(err: GmsAdError) {
                        Log.w(TAG, "rewarded failed to show: ${err.code} ${err.message}")
                        cont.resume(RewardedOutcome(false, "", 0u))
                    }
                }
                entry.show(activity) { reward ->
                    granted = RewardedOutcome(
                        granted = true,
                        rewardType = reward.type,
                        rewardAmount = reward.amount.toUInt(),
                    )
                }
            }
        }
    }

    override suspend fun show_banner(request: BannerRequest): NativeHandleId {
        val handleId = IstmoRuntime.allocHandleId(AdMobDispatcher.PLUGIN_ID)
        withContext(Dispatchers.Main) {
            val view = AdView(activity)
            view.adUnitId = request.adUnitId
            val adSize = pickAdSize(request.rect)
            view.setAdSize(adSize)
            val paintedSizePx = adSizePixels(adSize)
            Log.i(
                TAG,
                "banner request unit=${request.adUnitId} rectPx=${request.rect.width}x${request.rect.height}@${request.rect.x},${request.rect.y} " +
                    "-> adSize=${adSize.width}x${adSize.height}dp = ${paintedSizePx.width}x${paintedSizePx.height}px",
            )
            view.adListener = object : AdListener() {
                override fun onAdLoaded() {
                    Log.i(TAG, "banner loaded (${request.adUnitId})")
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
            val popup = attachBannerAsPopup(view, request.rect, paintedSizePx)
            view.loadAd(AdRequest.Builder().build())
            banners[handleId] = BannerEntry(view, popup)
        }
        return handleId
    }

    override suspend fun update_banner(banner: NativeHandleId, rect: BannerRect) {
        val entry = banners[banner] ?: return
        withContext(Dispatchers.Main) {
            val painted = adSizePixels(entry.view.adSize ?: AdSize.BANNER)
            entry.popup.update(rect.x.toInt(), rect.y.toInt(), painted.width, painted.height)
        }
    }

    override suspend fun hide_banner(banner: NativeHandleId) {
        val entry = banners.remove(banner) ?: run {
            IstmoRuntime.forgetHandle(banner)
            return
        }
        IstmoRuntime.forgetHandle(banner)
        withContext(Dispatchers.Main) { tearDownBanner(entry) }
    }

    private data class PxSize(val width: Int, val height: Int)

    private fun attachBannerAsPopup(
        view: AdView,
        rect: BannerRect,
        painted: PxSize,
    ): PopupWindow {
        val host = FrameLayout(activity)
        host.addView(view, FrameLayout.LayoutParams(painted.width, painted.height))
        val popup = PopupWindow(host, painted.width, painted.height, false).apply {
            isClippingEnabled = false
            inputMethodMode = PopupWindow.INPUT_METHOD_NOT_NEEDED
            isTouchable = true
            isFocusable = false
        }
        val decor = activity.window.decorView
        if (decor.windowToken != null) {
            popup.showAtLocation(decor, Gravity.TOP or Gravity.START, rect.x.toInt(), rect.y.toInt())
        } else {
            decor.post {
                popup.showAtLocation(decor, Gravity.TOP or Gravity.START, rect.x.toInt(), rect.y.toInt())
            }
        }
        return popup
    }

    private fun tearDownBanner(entry: BannerEntry) {
        try {
            entry.popup.dismiss()
        } catch (t: Throwable) {
            Log.w(TAG, "banner popup dismiss threw: ${t.message}")
        }
        (entry.view.parent as? ViewGroup)?.removeView(entry.view)
        entry.view.destroy()
    }

    private fun pickAdSize(rect: BannerRect): AdSize {
        val density = activity.resources.displayMetrics.density
        val widthDp = (rect.width.toFloat() / density).toInt().coerceAtLeast(0)
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

    private fun mapLoadError(err: LoadAdError): AdError = when (err.code) {
        3 -> AdError.NoFill
        2 -> AdError.Network(err.message)
        1 -> AdError.InvalidRequest(err.message)
        else -> AdError.Internal("code=${err.code} ${err.message}")
    }
}

