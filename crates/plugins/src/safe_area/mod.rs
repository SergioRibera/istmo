//! Safe-area insets published by the host UI layer.
//!
//! Delivered as an early-event stream so subscribers attaching after
//! the first insets update see the current value immediately.
//!
//! [`SafeAreaPublisher`] (behind the `winit-publisher` feature) lets a
//! Rust-driven UI publish the insets itself: on iOS it reads the key
//! window of the active scene, elsewhere it publishes nothing and the
//! host (Android's window-insets listener, a desktop shell) is expected
//! to publish instead.

#[cfg(feature = "winit-publisher")]
pub mod publisher;

#[cfg(feature = "winit-publisher")]
pub use publisher::SafeAreaPublisher;

use istmo_core::IstmoError;
use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the safe-area stream.
pub const SAFE_AREA_PLUGIN_ID: &str = "istmo.safe_area";

/// Early-event channel the insets are published on.
pub const SAFE_AREA_CHANNEL: &str = "istmo.safe_area";

/// Distances from each window edge, in logical points.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeInsets {
    /// Top edge.
    pub top: f32,
    /// Right edge.
    pub right: f32,
    /// Bottom edge.
    pub bottom: f32,
    /// Left edge.
    pub left: f32,
}

impl EdgeInsets {
    /// No inset on any edge.
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// Edge-wise maximum of `self` and `other`.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        Self {
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
            left: self.left.max(other.left),
        }
    }

    /// Left plus right.
    #[must_use]
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// Top plus bottom.
    #[must_use]
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

/// Insets of everything that can cover the window.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SafeAreaInsets {
    /// Status and navigation bars.
    pub system_bars: EdgeInsets,
    /// On-screen keyboard.
    pub ime: EdgeInsets,
    /// Notches and camera cutouts.
    pub display_cutout: EdgeInsets,
}

impl SafeAreaInsets {
    /// Padding that keeps content clear of bars and cutouts.
    #[must_use]
    pub const fn padding(self) -> EdgeInsets {
        self.system_bars.max(self.display_cutout)
    }

    /// [`padding`](Self::padding), with the bottom also clear of the
    /// keyboard.
    #[must_use]
    pub const fn view_padding(self) -> EdgeInsets {
        let mut base = self.padding();
        base.bottom = base.bottom.max(self.ime.bottom);
        base
    }
}

impl LatestValue for SafeAreaInsets {
    const PLUGIN_ID: &'static str = SAFE_AREA_PLUGIN_ID;
    const CHANNEL: &'static str = SAFE_AREA_CHANNEL;
}

/// Reader for the insets the host publishes.
pub type SafeArea = LatestClient<SafeAreaInsets>;

/// Insets as they are published.
pub type SafeAreaStream = EarlyStream<SafeAreaInsets>;

impl LatestClient<SafeAreaInsets> {
    /// The current insets, or zero before the first publish or when the
    /// retained payload does not decode.
    #[deprecated(note = "use `current_or_default()`, which reports decode errors")]
    #[must_use]
    pub fn current_or_zero(&self) -> SafeAreaInsets {
        self.current_or_default().unwrap_or_else(|err: IstmoError| {
            tracing::warn!(target: "istmo.safe_area", %err, "undecodable insets");
            SafeAreaInsets::default()
        })
    }
}
