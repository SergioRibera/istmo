//! Google Mobile Ads (`AdMob`) plugin.
//!
//! # Sub-modules
//!
//! * [`slot`] — UI-agnostic [`BannerSlot`](slot::BannerSlot) that any Rust
//!   UI framework (egui, iced, slint, gpui, …) can drive with a
//!   two-line adapter.
//!
//! Exposes the three ad formats worth being an istmo primitive:
//!
//! * **Interstitial** — full-screen ad shown between app screens. Load
//!   once, show later. Terminal outcomes: `Shown` (user viewed and
//!   dismissed) / `Dismissed` (closed early) / `FailedToShow`.
//! * **Rewarded** — full-screen video that grants the app a reward
//!   (coins / lives / unlock) once the user watches to completion.
//! * **Banner overlay** — an `AdView` docked over the Rust-driven UI
//!   surface. Rust owns positioning: the plugin call ships an
//!   [`AdMob::show_banner`] with a `BannerRect` in *physical pixels*
//!   from the top-left of the app window. Rust can move / hide /
//!   re-show at will.
//!
//! Every load call returns a [`NativeHandleId`] that Rust adopts into a
//! typed [`NativeHandle<T>`] (see [`AdMobClient::load_interstitial_owned`]
//! and friends). Native ownership is tied to the handle's lifetime:
//! dropping the handle without showing / hiding fires
//! `Frame::ReleaseNativeHandle` and the backend releases the ad.
//!
//! Wire id is `istmo.admob` under the `istmo.` core namespace. The
//! plugin is stateful (`init = AdMobConfig`) — the config initialises
//! the Mobile Ads SDK exactly once per process.
//!
//! Native ads / custom rendering — deliberately out of scope. `AdMob`s
//! policy requires impression tracking hooks that only make sense
//! against a `NativeAdView` (Android) / `GADNativeAdView` (iOS); asking
//! Rust to reimplement view-tracking would drift the impression counts
//! and violate the terms of service.

pub mod slot;

use std::sync::Arc;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId, Runtime};
use istmo_macros::plugin;

/// Wire identifier of the `AdMob` plugin.
pub const ADMOB_PLUGIN_ID: &str = "istmo.admob";

/// Phantom marker for the handle type of a loaded interstitial ad.
#[non_exhaustive]
#[derive(Debug)]
pub enum Interstitial {}

/// Phantom marker for the handle type of a loaded rewarded ad.
#[non_exhaustive]
#[derive(Debug)]
pub enum Rewarded {}

/// Phantom marker for the handle type of a live banner overlay.
#[non_exhaustive]
#[derive(Debug)]
pub enum Banner {}

// `AdMobConfig`, `BannerRect`, `BannerRequest`, `InterstitialOutcome`,
// `RewardedOutcome`, `AdError` are generated from the canonical
// `Contract` builder in `istmo-plugins-schema` via `build.rs`.
include!(concat!(env!("OUT_DIR"), "/admob_types.rs"));

impl std::fmt::Display for AdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotInitialized => f.write_str("Mobile Ads SDK not initialised"),
            Self::NoFill => f.write_str("ad request returned no fill"),
            Self::Network(msg) => write!(f, "ad network error: {msg}"),
            Self::InvalidRequest(msg) => write!(f, "invalid ad request: {msg}"),
            Self::UnknownAd => f.write_str("unknown or stale ad handle"),
            Self::Internal(msg) => write!(f, "ad backend error: {msg}"),
        }
    }
}

impl std::error::Error for AdError {}

/// Google Mobile Ads (`AdMob`) plugin surface.
#[plugin(
    name = "istmo.admob",
    init = AdMobConfig,
    crate = "::istmo_core",
)]
pub trait AdMob {
    /// Preload an interstitial ad. Returns a `NativeHandleId` that
    /// [`Self::show_interstitial`] consumes.
    async fn load_interstitial(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    /// Present a previously-loaded interstitial. Resolves once the user
    /// dismisses the ad. The handle is consumed by this call — the
    /// backend removes it from its registry.
    async fn show_interstitial(&self, ad: NativeHandleId) -> Result<InterstitialOutcome, AdError>;

    /// Preload a rewarded ad. Returns a `NativeHandleId` for
    /// [`Self::show_rewarded`].
    async fn load_rewarded(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    /// Present a previously-loaded rewarded ad. Resolves with a
    /// [`RewardedOutcome`] carrying the reward payload. Handle is
    /// consumed.
    async fn show_rewarded(&self, ad: NativeHandleId) -> Result<RewardedOutcome, AdError>;

    /// Show a banner overlay at `request.rect` (physical pixels,
    /// top-left origin of the app window). The banner stays visible
    /// until [`Self::hide_banner`] or the returned handle is dropped.
    async fn show_banner(&self, request: BannerRequest) -> Result<NativeHandleId, AdError>;

    /// Move or resize a live banner. No-op if the handle is unknown.
    async fn update_banner(&self, banner: NativeHandleId, rect: BannerRect) -> Result<(), AdError>;

    /// Hide + release a banner. Handle is consumed.
    async fn hide_banner(&self, banner: NativeHandleId) -> Result<(), AdError>;
}

impl AdMobClient {
    /// Runtime bound to this client — same accessor as `SignInClient::runtime`.
    /// Handy for adopting `NativeHandleId`s returned from raw trait
    /// methods into typed handles.
    #[must_use]
    pub const fn runtime(&self) -> &Arc<Runtime> {
        &self.__runtime
    }

    /// Ergonomic wrapper: load an interstitial and adopt the returned id
    /// into a typed [`NativeHandle<Interstitial>`] tied to this client's
    /// runtime. Dropping the handle without calling
    /// [`Self::show_interstitial_owned`] fires
    /// `Frame::ReleaseNativeHandle`.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::load_interstitial`].
    pub async fn load_interstitial_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Interstitial>, IstmoError> {
        let id = self.load_interstitial(ad_unit_id).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    /// Show a previously-loaded interstitial. Consumes the handle
    /// (`into_id`) so no release frame fires on drop.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::show_interstitial`].
    pub async fn show_interstitial_owned(
        &self,
        ad: NativeHandle<Interstitial>,
    ) -> Result<InterstitialOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_interstitial(id).await
    }

    /// Load a rewarded ad and adopt.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::load_rewarded`].
    pub async fn load_rewarded_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Rewarded>, IstmoError> {
        let id = self.load_rewarded(ad_unit_id).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    /// Show a rewarded ad. Consumes the handle.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::show_rewarded`].
    pub async fn show_rewarded_owned(
        &self,
        ad: NativeHandle<Rewarded>,
    ) -> Result<RewardedOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_rewarded(id).await
    }

    /// Show a banner overlay and adopt the returned id.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::show_banner`].
    pub async fn show_banner_owned(
        &self,
        request: BannerRequest,
    ) -> Result<NativeHandle<Banner>, IstmoError> {
        let id = self.show_banner(request).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    /// Move / resize a live banner referenced by an owned handle.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::update_banner`].
    pub async fn update_banner_owned(
        &self,
        banner: &NativeHandle<Banner>,
        rect: BannerRect,
    ) -> Result<(), IstmoError> {
        self.update_banner(banner.id(), rect).await
    }

    /// Hide + release a banner. Consumes the handle.
    ///
    /// # Errors
    /// Propagates every [`AdError`] from [`AdMob::hide_banner`].
    pub async fn hide_banner_owned(&self, banner: NativeHandle<Banner>) -> Result<(), IstmoError> {
        let id = banner.into_id();
        self.hide_banner(id).await
    }
}
