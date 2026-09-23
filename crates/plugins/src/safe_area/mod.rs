//! Safe-area insets published by the host UI layer.
//!
//! Delivered as an early-event stream so subscribers attaching after
//! the first insets update see the current value immediately.
//!
//! A `SafeAreaPublisher` (behind the `winit-publisher` feature) is
//! provided for winit-based desktops that want to synthesise insets
//! from window insets.

#[cfg(feature = "winit-publisher")]
pub mod publisher;

#[cfg(feature = "winit-publisher")]
pub use publisher::SafeAreaPublisher;

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const SAFE_AREA_CHANNEL: &str = "istmo.safe_area";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl EdgeInsets {
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        Self {
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
            left: self.left.max(other.left),
        }
    }

    #[must_use]
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    #[must_use]
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SafeAreaInsets {
    pub system_bars: EdgeInsets,

    pub ime: EdgeInsets,

    pub display_cutout: EdgeInsets,
}

impl SafeAreaInsets {
    #[must_use]
    pub const fn padding(self) -> EdgeInsets {
        self.system_bars.max(self.display_cutout)
    }

    #[must_use]
    pub const fn view_padding(self) -> EdgeInsets {
        let mut base = self.padding();
        base.bottom = base.bottom.max(self.ime.bottom);
        base
    }
}

#[derive(Debug, Clone)]
pub struct SafeArea {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for SafeArea {
    const PLUGIN_ID: &'static str = "istmo.safe_area";
}

impl SafeArea {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(SAFE_AREA_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<SafeAreaInsets>, IstmoError> {
        match self.slot.peek() {
            None => Ok(None),
            Some(bytes) => {
                let (insets, _) = codec::decode::<SafeAreaInsets>(&bytes)?;
                Ok(Some(insets))
            }
        }
    }

    #[must_use]
    pub fn current_or_zero(&self) -> SafeAreaInsets {
        self.current().ok().flatten().unwrap_or_default()
    }

    #[must_use]
    pub fn stream(&self) -> SafeAreaStream {
        SafeAreaStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct SafeAreaStream {
    rx: Receiver<Vec<u8>>,
}

impl SafeAreaStream {
    pub fn recv(&self) -> Result<SafeAreaInsets, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<SafeAreaInsets, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<SafeAreaInsets, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<SafeAreaInsets, IstmoError> {
    let (insets, _) = codec::decode::<SafeAreaInsets>(bytes)?;
    Ok(insets)
}
