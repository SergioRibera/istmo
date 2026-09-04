// Swift mirrors of the Rust `#[message]` types in
// `istmo-plugins/src/admob/mod.rs`. `NativeHandleId` is defined once in
// SignInTypes.swift; we rely on that shared alias.

import Foundation

public struct AdMobConfig {
    public let appId: String
    public let testDeviceIds: [String]
    public let childDirectedTreatment: Bool

    public init(appId: String, testDeviceIds: [String], childDirectedTreatment: Bool) {
        self.appId = appId
        self.testDeviceIds = testDeviceIds
        self.childDirectedTreatment = childDirectedTreatment
    }
}

/// Rectangle in *physical pixels* (top-left origin) — the Rust side
/// picks pixel coordinates from its own layout system (egui). The
/// backend converts to UIKit points before positioning the banner.
public struct BannerRect {
    public let x: UInt32
    public let y: UInt32
    public let width: UInt32
    public let height: UInt32

    public init(x: UInt32, y: UInt32, width: UInt32, height: UInt32) {
        self.x = x
        self.y = y
        self.width = width
        self.height = height
    }
}

public struct BannerRequest {
    public let adUnitId: String
    public let rect: BannerRect

    public init(adUnitId: String, rect: BannerRect) {
        self.adUnitId = adUnitId
        self.rect = rect
    }
}

public enum InterstitialOutcome: UInt32 {
    case dismissed = 0
    case failedToShow = 1
}

public struct RewardedOutcome {
    public let granted: Bool
    public let rewardType: String
    public let rewardAmount: UInt32

    public init(granted: Bool, rewardType: String, rewardAmount: UInt32) {
        self.granted = granted
        self.rewardType = rewardType
        self.rewardAmount = rewardAmount
    }
}

public enum AdError: Error {
    case notInitialized
    case noFill
    case network(String)
    case invalidRequest(String)
    case unknownAd
    case internalError(String)
}
