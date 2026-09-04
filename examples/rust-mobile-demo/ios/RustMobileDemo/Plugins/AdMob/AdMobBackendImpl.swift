// iOS impl of `AdMobBackend` using Google Mobile Ads SDK v11.x.
//
// Three ad formats:
//
// * **Interstitial** — `GADInterstitialAd.load(withAdUnitID:...)` +
//   `present(fromRootViewController:)`. The load is async; the show
//   returns once the ad is dismissed via a `GADFullScreenContentDelegate`.
// * **Rewarded** — `GADRewardedAd.load` + `present(fromRootViewController:
//   userDidEarnRewardHandler:)`. Same lifecycle as interstitial plus a
//   reward callback that fires before dismissal.
// * **Banner** — `GADBannerView` added to the app window's root view.
//   Positioning uses the `BannerRect` (physical pixels) converted to
//   UIKit points via `UIScreen.main.scale`.
//
// Every `load_*` and `show_banner` call mints a `NativeHandleId` via
// `IstmoRuntime.shared.allocHandleId`. The backend stores the SDK object
// under that id in `interstitials` / `rewardeds` / `banners`, and forgets
// the id when the ad is consumed (shown / dismissed) or hidden. Rust's
// `NativeHandle<T>` drop path also routes through `HandleReleaser` to
// clean up if Rust drops the handle without calling `show_*` / `hide_*`.
//
// SDK init happens in `AdMobFactoryImpl.create`, exactly once per process.

import Foundation
import IstmoRuntime
#if canImport(GoogleMobileAds)
import GoogleMobileAds
import UIKit
#endif

public final class AdMobFactoryImpl: AdMobFactory {
    public init() {}

    public func create(config: AdMobConfig) async throws -> AdMobBackend {
        #if canImport(GoogleMobileAds)
        // Apply test device ids + child-directed flag BEFORE `start` so
        // the initial ad request the SDK issues honours them.
        let cfg = MobileAds.shared.requestConfiguration
        cfg.testDeviceIdentifiers = config.testDeviceIds
        cfg.tagForChildDirectedTreatment = config.childDirectedTreatment
            ? NSNumber(value: true)
            : NSNumber(value: false)

        // `start(completionHandler:)` runs once per process; a repeat call
        // is a no-op. We await it so the first ad request sees an
        // initialised SDK.
        _ = await withCheckedContinuation { (cont: CheckedContinuation<Bool, Never>) in
            MobileAds.shared.start { _ in cont.resume(returning: true) }
        }
        #endif
        return AdMobBackendImpl()
    }
}

public final class AdMobBackendImpl: AdMobBackend, HandleReleaser {

    private let lock = NSLock()
    // The underlying types are stored as `AnyObject` so the file compiles
    // when GoogleMobileAds is not linked; at runtime the casts back in
    // each helper below are safe.
    private var interstitials: [NativeHandleId: AnyObject] = [:]
    private var rewardeds: [NativeHandleId: AnyObject] = [:]
    private var banners: [NativeHandleId: AnyObject] = [:]

    public init() {}

    // MARK: - Interstitial

    public func load_interstitial(ad_unit_id: String) async throws -> NativeHandleId {
        #if canImport(GoogleMobileAds)
        let request = Request()
        do {
            let ad = try await InterstitialAd.load(with: ad_unit_id, request: request)
            let id = IstmoRuntime.shared.allocHandleId(pluginId: AdMobDispatcher.PLUGIN_ID)
            lock.withLock { interstitials[id] = ad }
            return id
        } catch {
            throw Self.mapAdError(error)
        }
        #else
        throw AdError.notInitialized
        #endif
    }

    public func show_interstitial(ad: NativeHandleId) async throws -> InterstitialOutcome {
        #if canImport(GoogleMobileAds)
        let entry = lock.withLock { interstitials[ad] } as? InterstitialAd
        guard let entry = entry else { throw AdError.unknownAd }
        let presenter = await Self.rootViewController()
        guard let presenter = presenter else {
            throw AdError.internalError("no root view controller for interstitial present")
        }
        let coordinator = InterstitialCoordinator()
        entry.fullScreenContentDelegate = coordinator
        let outcome = await withCheckedContinuation { (cont: CheckedContinuation<InterstitialOutcome, Never>) in
            coordinator.continuation = cont
            DispatchQueue.main.async {
                entry.present(from: presenter)
            }
        }
        lock.withLock { _ = interstitials.removeValue(forKey: ad) }
        IstmoRuntime.shared.forgetHandle(ad)
        return outcome
        #else
        throw AdError.notInitialized
        #endif
    }

    // MARK: - Rewarded

    public func load_rewarded(ad_unit_id: String) async throws -> NativeHandleId {
        #if canImport(GoogleMobileAds)
        let request = Request()
        do {
            let ad = try await RewardedAd.load(with: ad_unit_id, request: request)
            let id = IstmoRuntime.shared.allocHandleId(pluginId: AdMobDispatcher.PLUGIN_ID)
            lock.withLock { rewardeds[id] = ad }
            return id
        } catch {
            throw Self.mapAdError(error)
        }
        #else
        throw AdError.notInitialized
        #endif
    }

    public func show_rewarded(ad: NativeHandleId) async throws -> RewardedOutcome {
        #if canImport(GoogleMobileAds)
        let entry = lock.withLock { rewardeds[ad] } as? RewardedAd
        guard let entry = entry else { throw AdError.unknownAd }
        let presenter = await Self.rootViewController()
        guard let presenter = presenter else {
            throw AdError.internalError("no root view controller for rewarded present")
        }
        let coordinator = RewardedCoordinator()
        entry.fullScreenContentDelegate = coordinator
        let outcome = await withCheckedContinuation { (cont: CheckedContinuation<RewardedOutcome, Never>) in
            coordinator.continuation = cont
            DispatchQueue.main.async {
                entry.present(from: presenter) {
                    let reward = entry.adReward
                    coordinator.granted = true
                    coordinator.rewardType = reward.type
                    coordinator.rewardAmount = UInt32(truncating: reward.amount)
                }
            }
        }
        lock.withLock { _ = rewardeds.removeValue(forKey: ad) }
        IstmoRuntime.shared.forgetHandle(ad)
        return outcome
        #else
        throw AdError.notInitialized
        #endif
    }

    // MARK: - Banner

    public func show_banner(request: BannerRequest) async throws -> NativeHandleId {
        #if canImport(GoogleMobileAds)
        let id = IstmoRuntime.shared.allocHandleId(pluginId: AdMobDispatcher.PLUGIN_ID)
        return try await MainActor.run { [weak self] in
            guard let self = self, let root = Self.rootViewControllerNow() else {
                throw AdError.internalError("no root view controller for banner attach")
            }
            let scale = UIScreen.main.scale
            let frame = CGRect(
                x: CGFloat(request.rect.x) / scale,
                y: CGFloat(request.rect.y) / scale,
                width: CGFloat(request.rect.width) / scale,
                height: CGFloat(request.rect.height) / scale,
            )
            let banner = BannerView(adSize: AdSizeFromCGSize(frame.size))
            banner.adUnitID = request.adUnitId
            banner.rootViewController = root
            banner.frame = frame
            banner.load(Request())
            root.view.addSubview(banner)
            self.lock.withLock { self.banners[id] = banner }
            return id
        }
        #else
        throw AdError.notInitialized
        #endif
    }

    public func update_banner(banner: NativeHandleId, rect: BannerRect) async throws {
        #if canImport(GoogleMobileAds)
        let view = lock.withLock { banners[banner] } as? BannerView
        guard let view = view else { return } // hide_banner-race: silently ignore
        await MainActor.run {
            let scale = UIScreen.main.scale
            view.frame = CGRect(
                x: CGFloat(rect.x) / scale,
                y: CGFloat(rect.y) / scale,
                width: CGFloat(rect.width) / scale,
                height: CGFloat(rect.height) / scale,
            )
        }
        #endif
    }

    public func hide_banner(banner: NativeHandleId) async throws {
        #if canImport(GoogleMobileAds)
        let view = lock.withLock { banners.removeValue(forKey: banner) } as? BannerView
        IstmoRuntime.shared.forgetHandle(banner)
        guard let view = view else { return }
        await MainActor.run { view.removeFromSuperview() }
        #endif
    }

    // MARK: - HandleReleaser

    public func releaseNativeHandle(_ handleId: UInt64) {
        // Drop from every map; the id only lives in one at a time but
        // this way `HandleReleaser` stays branch-free.
        let interstitial = lock.withLock { interstitials.removeValue(forKey: handleId) }
        let rewarded = lock.withLock { rewardeds.removeValue(forKey: handleId) }
        let banner = lock.withLock { banners.removeValue(forKey: handleId) }
        #if canImport(GoogleMobileAds)
        if let bannerView = banner as? BannerView {
            DispatchQueue.main.async { bannerView.removeFromSuperview() }
        }
        _ = interstitial // released by ARC once we drop the last reference
        _ = rewarded
        #else
        _ = interstitial
        _ = rewarded
        _ = banner
        #endif
    }

    // MARK: - Helpers

    #if canImport(GoogleMobileAds)
    private static func mapAdError(_ error: Error) -> AdError {
        let nserror = error as NSError
        // GoogleMobileAds errors live under `com.google.admob` on iOS and
        // encode fine-grained codes in `code`. See `GADErrorCode`.
        if nserror.domain == "com.google.admob" || nserror.domain.contains("MobileAdsSDK") {
            switch nserror.code {
            case 1: return .invalidRequest(nserror.localizedDescription)   // GADErrorInvalidRequest
            case 2: return .network(nserror.localizedDescription)          // GADErrorNoFillOrInternal
            case 3: return .noFill
            default: return .internalError(nserror.localizedDescription)
            }
        }
        if nserror.domain == NSURLErrorDomain {
            return .network(nserror.localizedDescription)
        }
        return .internalError(nserror.localizedDescription)
    }

    @MainActor
    private static func rootViewControllerNow() -> UIViewController? {
        for scene in UIApplication.shared.connectedScenes.compactMap({ $0 as? UIWindowScene }) {
            for window in scene.windows where window.isKeyWindow {
                return window.rootViewController
            }
        }
        return nil
    }

    private static func rootViewController() async -> UIViewController? {
        await MainActor.run { rootViewControllerNow() }
    }
    #endif
}

// MARK: - Full-screen ad coordinators

#if canImport(GoogleMobileAds)

/// Bridges `GADFullScreenContentDelegate` callbacks to a checked
/// continuation. The instance stays alive because the ad object retains
/// its delegate; when the ad is done, ARC drops both together.
private final class InterstitialCoordinator: NSObject, FullScreenContentDelegate {

    var continuation: CheckedContinuation<InterstitialOutcome, Never>?

    func ad(_ ad: FullScreenPresentingAd, didFailToPresentFullScreenContentWithError error: Error) {
        continuation?.resume(returning: .failedToShow)
        continuation = nil
    }

    func adDidDismissFullScreenContent(_ ad: FullScreenPresentingAd) {
        continuation?.resume(returning: .dismissed)
        continuation = nil
    }
}

/// Same shape as `InterstitialCoordinator` plus reward fields the reward
/// handler on `present` writes before dismissal.
private final class RewardedCoordinator: NSObject, FullScreenContentDelegate {

    var continuation: CheckedContinuation<RewardedOutcome, Never>?
    var granted: Bool = false
    var rewardType: String = ""
    var rewardAmount: UInt32 = 0

    func ad(_ ad: FullScreenPresentingAd, didFailToPresentFullScreenContentWithError error: Error) {
        continuation?.resume(returning: RewardedOutcome(
            granted: false, rewardType: "", rewardAmount: 0,
        ))
        continuation = nil
    }

    func adDidDismissFullScreenContent(_ ad: FullScreenPresentingAd) {
        continuation?.resume(returning: RewardedOutcome(
            granted: granted, rewardType: rewardType, rewardAmount: rewardAmount,
        ))
        continuation = nil
    }
}

#endif

// MARK: - Small utilities

private extension NSLock {
    @discardableResult
    func withLock<T>(_ body: () -> T) -> T {
        lock()
        defer { unlock() }
        return body()
    }
}

