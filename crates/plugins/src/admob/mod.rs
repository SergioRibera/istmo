//! Google AdMob integration.
//!
//! Exposes banner, interstitial and rewarded ad surfaces via typed
//! [`NativeHandle`]s. The [`slot`] submodule provides a small
//! state-machine helper for driving a banner slot lifecycle from UI
//! code.

pub mod slot;

use istmo_core::{IstmoError, NativeHandle, NativeHandleId};
use istmo_macros::plugin;

/// Wire identifier for the AdMob plugin.
pub const ADMOB_PLUGIN_ID: &str = "istmo.admob";

#[non_exhaustive]
#[derive(Debug)]
pub enum Interstitial {}

#[non_exhaustive]
#[derive(Debug)]
pub enum Rewarded {}

#[non_exhaustive]
#[derive(Debug)]
pub enum Banner {}

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

#[plugin(
    name = "istmo.admob",
    init = AdMobConfig,
    crate = "::istmo_core",
)]
pub trait AdMob {

    async fn load_interstitial(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    async fn show_interstitial(&self, ad: NativeHandleId) -> Result<InterstitialOutcome, AdError>;

    async fn load_rewarded(&self, ad_unit_id: String) -> Result<NativeHandleId, AdError>;

    async fn show_rewarded(&self, ad: NativeHandleId) -> Result<RewardedOutcome, AdError>;

    async fn show_banner(&self, request: BannerRequest) -> Result<NativeHandleId, AdError>;

    async fn update_banner(&self, banner: NativeHandleId, rect: BannerRect) -> Result<(), AdError>;

    async fn hide_banner(&self, banner: NativeHandleId) -> Result<(), AdError>;
}

impl AdMobClient {

    pub async fn load_interstitial_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Interstitial>, IstmoError> {
        let id = self.load_interstitial(ad_unit_id).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    pub async fn show_interstitial_owned(
        &self,
        ad: NativeHandle<Interstitial>,
    ) -> Result<InterstitialOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_interstitial(id).await
    }

    pub async fn load_rewarded_owned(
        &self,
        ad_unit_id: String,
    ) -> Result<NativeHandle<Rewarded>, IstmoError> {
        let id = self.load_rewarded(ad_unit_id).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    pub async fn show_rewarded_owned(
        &self,
        ad: NativeHandle<Rewarded>,
    ) -> Result<RewardedOutcome, IstmoError> {
        let id = ad.into_id();
        self.show_rewarded(id).await
    }

    pub async fn show_banner_owned(
        &self,
        request: BannerRequest,
    ) -> Result<NativeHandle<Banner>, IstmoError> {
        let id = self.show_banner(request).await?;
        Ok(NativeHandle::adopt(&self.__runtime, id))
    }

    pub async fn update_banner_owned(
        &self,
        banner: &NativeHandle<Banner>,
        rect: BannerRect,
    ) -> Result<(), IstmoError> {
        self.update_banner(banner.id(), rect).await
    }

    pub async fn hide_banner_owned(&self, banner: NativeHandle<Banner>) -> Result<(), IstmoError> {
        let id = banner.into_id();
        self.hide_banner(id).await
    }
}

