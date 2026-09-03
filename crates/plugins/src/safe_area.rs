//! Safe-area insets plugin, Flutter-style.
//!
//! Every mobile app needs to know how much of the viewport is covered by
//! system chrome — status bar, navigation bar, notches, cutouts, and the
//! transient IME (keyboard). This plugin mirrors Flutter's `MediaQuery`
//! model: the native side subscribes to the OS inset callbacks
//! (`WindowInsets` on Android, `safeAreaInsets` + keyboard notifications
//! on iOS) and publishes a single `SafeAreaInsets` record to the runtime's
//! [`LatestValueSlot`] under [`SAFE_AREA_CHANNEL`].
//!
//! Everything is in **logical points** (dp / Flutter "logical pixels") so
//! the numbers drop straight into egui — egui's `pixels_per_point` equals
//! the platform's density factor, so 1 point == 1 dp on Android and 1
//! logical pixel on iOS.
//!
//! Fields:
//!
//! * [`SafeAreaInsets::system_bars`] — persistent system UI (status bar,
//!   navigation bar). Analogue of Flutter's `MediaQueryData.padding`.
//! * [`SafeAreaInsets::ime`] — transient keyboard overlay height, expressed
//!   as bottom inset only. Analogue of Flutter's `viewInsets.bottom`.
//! * [`SafeAreaInsets::display_cutout`] — display cutout (notches,
//!   punch-holes, dynamic island). Separate from `system_bars` so callers
//!   that draw edge-to-edge can respect only the cutout without losing
//!   the whole system-bar area.
//!
//! [`LatestValueSlot`]: istmo_core::early_events::LatestValueSlot

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

/// Early-event channel key the runtime publishes safe-area updates to.
pub const SAFE_AREA_CHANNEL: &str = "istmo.safe_area";

/// One rectangle of insets in logical points. `right` and `bottom` are
/// distances *from* the corresponding viewport edges — same convention
/// Flutter and CSS use.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct EdgeInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl EdgeInsets {
    /// Zero insets on every edge.
    pub const ZERO: Self = Self {
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
        left: 0.0,
    };

    /// Element-wise maximum. Handy for combining `system_bars` with
    /// `display_cutout` when the caller wants the union — same behaviour
    /// Flutter's `MediaQueryData.padding` gives.
    #[must_use]
    pub const fn max(self, other: Self) -> Self {
        Self {
            top: self.top.max(other.top),
            right: self.right.max(other.right),
            bottom: self.bottom.max(other.bottom),
            left: self.left.max(other.left),
        }
    }

    /// Total horizontal reserved space (`left + right`).
    #[must_use]
    pub fn horizontal(self) -> f32 {
        self.left + self.right
    }

    /// Total vertical reserved space (`top + bottom`).
    #[must_use]
    pub fn vertical(self) -> f32 {
        self.top + self.bottom
    }
}

/// Full safe-area snapshot published by the platform.
///
/// Mirror of Flutter's `MediaQueryData` split into three concerns. Callers
/// pick which slice to respect based on how they draw:
///
/// * A conventional layout that wants to stay clear of every OS chrome
///   should use `system_bars.max(display_cutout)` for its padding, and
///   add `ime.bottom` when the keyboard is up.
/// * An edge-to-edge design (banner, video background) may want only
///   `display_cutout` so the status/nav bar overlay stays translucent
///   above the content.
#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct SafeAreaInsets {
    /// Persistent system UI insets (status bar + navigation bar).
    pub system_bars: EdgeInsets,
    /// Transient IME (keyboard) overlay. Only the `bottom` component is
    /// meaningful today; other edges are reserved for future keyboard
    /// modes (split, floating).
    pub ime: EdgeInsets,
    /// Display cutout / notch / punch-hole area.
    pub display_cutout: EdgeInsets,
}

impl SafeAreaInsets {
    /// Flutter-style "padding" — the union of system bars and display
    /// cutout. This is what most callers want as the default safe zone.
    #[must_use]
    pub const fn padding(self) -> EdgeInsets {
        self.system_bars.max(self.display_cutout)
    }

    /// Flutter-style "view padding + view insets" — includes the keyboard
    /// overlay on the bottom edge. Use this when laying out something
    /// that must stay above the IME.
    #[must_use]
    pub const fn view_padding(self) -> EdgeInsets {
        let mut base = self.padding();
        base.bottom = base.bottom.max(self.ime.bottom);
        base
    }
}

/// Client for the [`SAFE_AREA_CHANNEL`] early-event slot.
#[derive(Debug, Clone)]
pub struct SafeArea {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for SafeArea {
    const PLUGIN_ID: &'static str = "istmo.safe_area";
}

impl SafeArea {
    /// Attaches to the process-global runtime.
    ///
    /// # Errors
    /// Returns [`IstmoError`] variants forwarded from
    /// [`Runtime::global`] and [`Self::from_runtime`].
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    /// Attaches to a specific runtime instance.
    ///
    /// # Errors
    /// Returns [`IstmoError::PluginNotDeclared`] when the runtime enforces
    /// declarations and this plugin id is missing from its `plugins:` list.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(SAFE_AREA_CHANNEL),
        })
    }

    /// Returns the currently retained snapshot, if any inset event has
    /// been published yet. Returns `None` early in the process lifetime
    /// before the platform has fired its first inset callback.
    ///
    /// # Errors
    /// Returns a codec error if the retained bytes cannot be decoded.
    pub fn current(&self) -> Result<Option<SafeAreaInsets>, IstmoError> {
        match self.slot.peek() {
            None => Ok(None),
            Some(bytes) => {
                let (insets, _) = codec::decode::<SafeAreaInsets>(&bytes)?;
                Ok(Some(insets))
            }
        }
    }

    /// Convenience: return the retained snapshot or a zero-insets record
    /// when nothing has been published yet. Handy for UI code that would
    /// otherwise have to unwrap `Option` on every frame.
    #[must_use]
    pub fn current_or_zero(&self) -> SafeAreaInsets {
        self.current().ok().flatten().unwrap_or_default()
    }

    /// Subscribes to inset updates. The returned stream first yields the
    /// currently retained snapshot (if any), then every subsequent update.
    #[must_use]
    pub fn stream(&self) -> SafeAreaStream {
        SafeAreaStream {
            rx: self.slot.subscribe(),
        }
    }
}

/// Subscription handle returned by [`SafeArea::stream`].
#[derive(Debug)]
pub struct SafeAreaStream {
    rx: Receiver<Vec<u8>>,
}

impl SafeAreaStream {
    /// Blocks until the next inset update arrives.
    ///
    /// # Errors
    /// Returns [`IstmoError::ChannelClosed`] when the underlying slot has
    /// been dropped by the runtime.
    pub fn recv(&self) -> Result<SafeAreaInsets, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Awaits the next inset update.
    ///
    /// # Errors
    /// Returns [`IstmoError::ChannelClosed`] when the underlying slot has
    /// been dropped by the runtime.
    pub async fn recv_async(&self) -> Result<SafeAreaInsets, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    /// Non-blocking read. `None` when the queue is empty.
    #[must_use]
    pub fn try_recv(&self) -> Option<Result<SafeAreaInsets, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<SafeAreaInsets, IstmoError> {
    let (insets, _) = codec::decode::<SafeAreaInsets>(bytes)?;
    Ok(insets)
}
