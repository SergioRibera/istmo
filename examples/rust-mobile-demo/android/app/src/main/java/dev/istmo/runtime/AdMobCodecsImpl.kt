package dev.istmo.runtime

import java.io.ByteArrayOutputStream

/**
 * Wire codecs for the `AdMob` plugin's `Named` types.
 *
 * Encoding rules mirror `crates/plugins/src/admob/mod.rs`:
 *
 * * `AdMobConfig` — String, Vec<String>, Bool.
 * * `BannerRect` — u32 varint ×4.
 * * `BannerRequest` — String, BannerRect.
 * * `InterstitialOutcome` — u32 varint discriminant.
 * * `RewardedOutcome` — Bool, String, u32.
 * * `AdError` — u32 varint discriminant + per-variant payload
 *   (`String` for Network / InvalidRequest / Internal).
 * * `NativeHandleId` — single u64 varint.
 */
class AdMobCodecsImpl : AdMobCodecs {

    // ---- AdMobConfig -----------------------------------------------------

    override fun readAdMobConfig(bytes: ByteArray, offset: Int): Bincode.Decoded<AdMobConfig> {
        var cursor = offset
        val appId = Bincode.readString(bytes, cursor); cursor = appId.consumed
        val ids = Bincode.readVec(bytes, cursor) { p, o -> Bincode.readString(p, o) }
        cursor = ids.consumed
        val child = Bincode.readBool(bytes, cursor); cursor = child.consumed
        return Bincode.Decoded(AdMobConfig(appId.value, ids.value, child.value), cursor)
    }

    override fun writeAdMobConfig(out: ByteArrayOutputStream, value: AdMobConfig) {
        Bincode.writeString(out, value.appId)
        Bincode.writeVec(out, value.testDeviceIds) { s, v -> Bincode.writeString(s, v) }
        Bincode.writeBool(out, value.childDirectedTreatment)
    }

    // ---- BannerRect / BannerRequest --------------------------------------

    override fun readBannerRect(bytes: ByteArray, offset: Int): Bincode.Decoded<BannerRect> {
        var cursor = offset
        val (x, c1) = Bincode.readVarintU64(bytes, cursor); cursor = c1
        val (y, c2) = Bincode.readVarintU64(bytes, cursor); cursor = c2
        val (w, c3) = Bincode.readVarintU64(bytes, cursor); cursor = c3
        val (h, c4) = Bincode.readVarintU64(bytes, cursor); cursor = c4
        return Bincode.Decoded(
            BannerRect(x.toUInt(), y.toUInt(), w.toUInt(), h.toUInt()),
            cursor,
        )
    }

    override fun writeBannerRect(out: ByteArrayOutputStream, value: BannerRect) {
        Bincode.writeVarintU64(out, value.x.toLong())
        Bincode.writeVarintU64(out, value.y.toLong())
        Bincode.writeVarintU64(out, value.width.toLong())
        Bincode.writeVarintU64(out, value.height.toLong())
    }

    override fun readBannerRequest(bytes: ByteArray, offset: Int): Bincode.Decoded<BannerRequest> {
        var cursor = offset
        val unit = Bincode.readString(bytes, cursor); cursor = unit.consumed
        val rect = readBannerRect(bytes, cursor); cursor = rect.consumed
        return Bincode.Decoded(BannerRequest(unit.value, rect.value), cursor)
    }

    override fun writeBannerRequest(out: ByteArrayOutputStream, value: BannerRequest) {
        Bincode.writeString(out, value.adUnitId)
        writeBannerRect(out, value.rect)
    }

    // ---- InterstitialOutcome / RewardedOutcome ---------------------------

    override fun readInterstitialOutcome(bytes: ByteArray, offset: Int): Bincode.Decoded<InterstitialOutcome> {
        val (disc, next) = Bincode.readVarintU64(bytes, offset)
        val variant = InterstitialOutcome.entries.getOrNull(disc.toInt())
            ?: error("InterstitialOutcome: unknown discriminant $disc")
        return Bincode.Decoded(variant, next)
    }

    override fun writeInterstitialOutcome(out: ByteArrayOutputStream, value: InterstitialOutcome) {
        Bincode.writeEnumDiscriminant(out, value.ordinal)
    }

    override fun readRewardedOutcome(bytes: ByteArray, offset: Int): Bincode.Decoded<RewardedOutcome> {
        var cursor = offset
        val granted = Bincode.readBool(bytes, cursor); cursor = granted.consumed
        val type = Bincode.readString(bytes, cursor); cursor = type.consumed
        val (amount, c3) = Bincode.readVarintU64(bytes, cursor); cursor = c3
        return Bincode.Decoded(
            RewardedOutcome(granted.value, type.value, amount.toUInt()),
            cursor,
        )
    }

    override fun writeRewardedOutcome(out: ByteArrayOutputStream, value: RewardedOutcome) {
        Bincode.writeBool(out, value.granted)
        Bincode.writeString(out, value.rewardType)
        Bincode.writeVarintU64(out, value.rewardAmount.toLong())
    }

    // ---- AdError ---------------------------------------------------------

    override fun readAdError(bytes: ByteArray, offset: Int): Bincode.Decoded<AdError> {
        var cursor = offset
        val (disc, next) = Bincode.readVarintU64(bytes, cursor); cursor = next
        return when (disc.toInt()) {
            0 -> Bincode.Decoded(AdError.NotInitialized, cursor)
            1 -> Bincode.Decoded(AdError.NoFill, cursor)
            2 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(AdError.Network(msg.value), msg.consumed)
            }
            3 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(AdError.InvalidRequest(msg.value), msg.consumed)
            }
            4 -> Bincode.Decoded(AdError.UnknownAd, cursor)
            5 -> {
                val msg = Bincode.readString(bytes, cursor)
                Bincode.Decoded(AdError.Internal(msg.value), msg.consumed)
            }
            else -> error("AdError: unknown discriminant $disc")
        }
    }

    override fun writeAdError(out: ByteArrayOutputStream, value: AdError) {
        when (value) {
            AdError.NotInitialized -> Bincode.writeEnumDiscriminant(out, 0)
            AdError.NoFill -> Bincode.writeEnumDiscriminant(out, 1)
            is AdError.Network -> {
                Bincode.writeEnumDiscriminant(out, 2)
                Bincode.writeString(out, value.message)
            }
            is AdError.InvalidRequest -> {
                Bincode.writeEnumDiscriminant(out, 3)
                Bincode.writeString(out, value.message)
            }
            AdError.UnknownAd -> Bincode.writeEnumDiscriminant(out, 4)
            is AdError.Internal -> {
                Bincode.writeEnumDiscriminant(out, 5)
                Bincode.writeString(out, value.message)
            }
        }
    }

    // ---- NativeHandleId --------------------------------------------------

    override fun readNativeHandleId(bytes: ByteArray, offset: Int): Bincode.Decoded<Long> {
        val (value, next) = Bincode.readVarintU64(bytes, offset)
        return Bincode.Decoded(value, next)
    }

    override fun writeNativeHandleId(out: ByteArrayOutputStream, value: Long) {
        Bincode.writeVarintU64(out, value)
    }
}
