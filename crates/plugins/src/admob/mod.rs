//! Google `AdMob` integration.
//!
//! Exposes banner, interstitial and rewarded ad surfaces via typed
//! [`NativeHandle`]s. The [`slot`] submodule provides a small
//! state-machine helper for driving a banner slot lifecycle from UI
//! code.

pub mod slot;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId};
use istmo_macros::plugin;

/// Wire identifier for the `AdMob` plugin.
pub const ADMOB_PLUGIN_ID: &str = "istmo.admob";

/// Marker for a loaded interstitial ad's [`NativeHandle`].
#[derive(Debug, Clone, Copy)]
pub struct Interstitial(std::convert::Infallible);

/// Marker for a loaded rewarded ad's [`NativeHandle`].
#[derive(Debug, Clone, Copy)]
pub struct Rewarded(std::convert::Infallible);

/// Marker for an on-screen banner's [`NativeHandle`].
#[derive(Debug, Clone, Copy)]
pub struct Banner(std::convert::Infallible);

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

/// Google Mobile Ads, hosted by the platform. Ads are referenced by
/// raw [`NativeHandleId`]s on the wire; [`AdMobClient`]'s `*_owned`
/// helpers wrap them in [`NativeHandle`]s that release the ad on drop.
#[plugin(
    name = "istmo.admob",
    init = AdMobConfig,
    crate = "::istmo_core",
)]
pub trait AdMob {
    /// Load an interstitial for `ad_unit_id`.
    async fn load_interstitial(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    /// Show a loaded interstitial; the platform frees it afterwards.
    async fn show_interstitial(&self, ad: NativeHandleId) -> Result<InterstitialOutcome, AdError>;

    /// Load a rewarded ad for `ad_unit_id`.
    async fn load_rewarded(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    /// Show a loaded rewarded ad; the platform frees it afterwards.
    async fn show_rewarded(&self, ad: NativeHandleId) -> Result<RewardedOutcome, AdError>;

    /// Load and show a banner at the requested rect.
    async fn show_banner(&self, request: BannerRequest) -> Result<NativeHandleId, AdError>;

    /// Move or resize a shown banner.
    async fn update_banner(&self, banner: NativeHandleId, rect: BannerRect) -> Result<(), AdError>;

    /// Remove a banner; the platform frees it.
    async fn hide_banner(&self, banner: NativeHandleId) -> Result<(), AdError>;
}

impl AdMobClient {
    /// [`load_interstitial`](Self::load_interstitial), adopting the ad.
    pub async fn load_interstitial_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Interstitial>, IstmoError> {
        let id = self.load_interstitial(ad_unit_id).await?;
        Ok(NativeHandle::adopt(self.runtime(), id))
    }

    /// [`show_interstitial`](Self::show_interstitial), handing the ad's
    /// ownership to the platform.
    pub async fn show_interstitial_owned(
        &self,
        ad: NativeHandle<Interstitial>,
    ) -> Result<InterstitialOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_interstitial(id).await
    }

    /// [`load_rewarded`](Self::load_rewarded), adopting the ad.
    pub async fn load_rewarded_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Rewarded>, IstmoError> {
        let id = self.load_rewarded(ad_unit_id).await?;
        Ok(NativeHandle::adopt(self.runtime(), id))
    }

    /// [`show_rewarded`](Self::show_rewarded), handing the ad's
    /// ownership to the platform.
    pub async fn show_rewarded_owned(
        &self,
        ad: NativeHandle<Rewarded>,
    ) -> Result<RewardedOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_rewarded(id).await
    }

    /// [`show_banner`](Self::show_banner), adopting the banner.
    pub async fn show_banner_owned(
        &self,
        request: BannerRequest,
    ) -> Result<NativeHandle<Banner>, IstmoError> {
        let id = self.show_banner(request).await?;
        Ok(NativeHandle::adopt(self.runtime(), id))
    }

    /// [`update_banner`](Self::update_banner) on an owned banner.
    pub async fn update_banner_owned(
        &self,
        banner: &NativeHandle<Banner>,
        rect: BannerRect,
    ) -> Result<(), IstmoError> {
        self.update_banner(banner.id(), rect).await
    }

    /// [`hide_banner`](Self::hide_banner), handing the banner's
    /// ownership to the platform.
    pub async fn hide_banner_owned(&self, banner: NativeHandle<Banner>) -> Result<(), IstmoError> {
        let id = banner.into_id();
        self.hide_banner(id).await
    }
}
