package dev.istmo.runtime

/**
 * Kotlin mirrors of the Rust `#[message]` types in
 * `istmo-plugins/src/admob/mod.rs`.
 *
 * Enum variant order must match the Rust declaration byte-for-byte —
 * bincode encodes enum discriminants as u32 varints in declaration
 * order, so a reshuffle here silently breaks the wire.
 */
data class AdMobConfig(
    val appId: String,
    val testDeviceIds: List<String>,
    val childDirectedTreatment: Boolean,
)

data class BannerRect(
    val x: UInt,
    val y: UInt,
    val width: UInt,
    val height: UInt,
)

data class BannerRequest(
    val adUnitId: String,
    val rect: BannerRect,
)

enum class InterstitialOutcome {
    Dismissed,
    FailedToShow,
}

data class RewardedOutcome(
    val granted: Boolean,
    val rewardType: String,
    val rewardAmount: UInt,
)

/** Domain error. Backends throw `BackendException(AdError.…)`; the
 *  dispatcher encodes it via `AdMobCodecsImpl.writeAdError`. */
sealed class AdError {
    object NotInitialized : AdError()
    object NoFill : AdError()
    data class Network(val message: String) : AdError()
    data class InvalidRequest(val message: String) : AdError()
    object UnknownAd : AdError()
    data class Internal(val message: String) : AdError()
}
