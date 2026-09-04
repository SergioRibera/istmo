// Codec impl for the `AdMob` plugin's `Named` types.
//
// Wire schema mirrors the Rust `#[message]` types verbatim:
//
// * `AdMobConfig` — String, Vec<String>, Bool.
// * `BannerRect` — u32×4.
// * `BannerRequest` — String, BannerRect.
// * `InterstitialOutcome` — u32 varint discriminant.
// * `RewardedOutcome` — Bool, String, u32.
// * `AdError` — u32 varint discriminant + String payload for the
//   Network / InvalidRequest / Internal variants.
// * `NativeHandleId` — u64 varint (shared alias with the SignIn plugin).

import Foundation
import IstmoRuntime

public final class AdMobCodecsImpl: AdMobCodecs {

    public init() {}

    // MARK: - AdMobConfig

    public func readAdMobConfig(_ c: inout Bincode.Cursor) throws -> AdMobConfig {
        let appId = try Bincode.readString(&c)
        let testIds = try Bincode.readVec(&c) { try Bincode.readString(&$0) }
        let childDirected = try Bincode.readBool(&c)
        return AdMobConfig(
            appId: appId,
            testDeviceIds: testIds,
            childDirectedTreatment: childDirected,
        )
    }

    public func writeAdMobConfig(_ out: inout Data, _ value: AdMobConfig) {
        Bincode.writeString(&out, value.appId)
        Bincode.writeVec(&out, value.testDeviceIds) { Bincode.writeString(&$0, $1) }
        Bincode.writeBool(&out, value.childDirectedTreatment)
    }

    // MARK: - BannerRect / BannerRequest

    public func readBannerRect(_ c: inout Bincode.Cursor) throws -> BannerRect {
        let x = try Bincode.readVarintU32(&c)
        let y = try Bincode.readVarintU32(&c)
        let w = try Bincode.readVarintU32(&c)
        let h = try Bincode.readVarintU32(&c)
        return BannerRect(x: x, y: y, width: w, height: h)
    }

    public func writeBannerRect(_ out: inout Data, _ value: BannerRect) {
        Bincode.writeVarintU32(&out, value.x)
        Bincode.writeVarintU32(&out, value.y)
        Bincode.writeVarintU32(&out, value.width)
        Bincode.writeVarintU32(&out, value.height)
    }

    public func readBannerRequest(_ c: inout Bincode.Cursor) throws -> BannerRequest {
        let unit = try Bincode.readString(&c)
        let rect = try readBannerRect(&c)
        return BannerRequest(adUnitId: unit, rect: rect)
    }

    public func writeBannerRequest(_ out: inout Data, _ value: BannerRequest) {
        Bincode.writeString(&out, value.adUnitId)
        writeBannerRect(&out, value.rect)
    }

    // MARK: - InterstitialOutcome / RewardedOutcome

    public func readInterstitialOutcome(_ c: inout Bincode.Cursor) throws -> InterstitialOutcome {
        let disc = try Bincode.readVarintU32(&c)
        guard let v = InterstitialOutcome(rawValue: disc) else {
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
        return v
    }

    public func writeInterstitialOutcome(_ out: inout Data, _ value: InterstitialOutcome) {
        Bincode.writeVarintU32(&out, value.rawValue)
    }

    public func readRewardedOutcome(_ c: inout Bincode.Cursor) throws -> RewardedOutcome {
        let granted = try Bincode.readBool(&c)
        let type = try Bincode.readString(&c)
        let amount = try Bincode.readVarintU32(&c)
        return RewardedOutcome(granted: granted, rewardType: type, rewardAmount: amount)
    }

    public func writeRewardedOutcome(_ out: inout Data, _ value: RewardedOutcome) {
        Bincode.writeBool(&out, value.granted)
        Bincode.writeString(&out, value.rewardType)
        Bincode.writeVarintU32(&out, value.rewardAmount)
    }

    // MARK: - AdError

    public func readAdError(_ c: inout Bincode.Cursor) throws -> AdError {
        let disc = try Bincode.readVarintU32(&c)
        switch disc {
        case 0: return .notInitialized
        case 1: return .noFill
        case 2: return .network(try Bincode.readString(&c))
        case 3: return .invalidRequest(try Bincode.readString(&c))
        case 4: return .unknownAd
        case 5: return .internalError(try Bincode.readString(&c))
        default:
            throw Bincode.DecodeError.invalidTag(UInt8(clamping: disc))
        }
    }

    public func writeAdError(_ out: inout Data, _ value: AdError) {
        switch value {
        case .notInitialized:
            Bincode.writeVarintU32(&out, 0)
        case .noFill:
            Bincode.writeVarintU32(&out, 1)
        case .network(let s):
            Bincode.writeVarintU32(&out, 2)
            Bincode.writeString(&out, s)
        case .invalidRequest(let s):
            Bincode.writeVarintU32(&out, 3)
            Bincode.writeString(&out, s)
        case .unknownAd:
            Bincode.writeVarintU32(&out, 4)
        case .internalError(let s):
            Bincode.writeVarintU32(&out, 5)
            Bincode.writeString(&out, s)
        }
    }

    // MARK: - NativeHandleId

    public func readNativeHandleId(_ c: inout Bincode.Cursor) throws -> NativeHandleId {
        try Bincode.readVarintU64(&c)
    }

    public func writeNativeHandleId(_ out: inout Data, _ value: NativeHandleId) {
        Bincode.writeVarintU64(&out, value)
    }
}
