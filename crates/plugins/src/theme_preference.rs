//! User-facing theme and accessibility preferences published by the
//! host platform.
//!
//! Native observers (`UITraitCollection` / `UIAccessibility` on Apple,
//! `AccessibilityManager` + `UIMode` on Android, `prefers-*` on Web,
//! `gsettings` on Linux, `SPI_GET*` on Windows) feed a single
//! [`ThemePreference`] snapshot through a latest-value slot so late
//! subscribers get the current state immediately.

use std::sync::Arc;

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub const THEME_PREFERENCE_PLUGIN_ID: &str = "istmo.theme_preference";
pub const THEME_PREFERENCE_CHANNEL: &str = "istmo.theme_preference";

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::struct_excessive_bools)]
pub struct ThemePreference {
    pub color_scheme: ColorScheme,

    /// Platform accent (`UIColor.tintColor` / Android 12 Material You /
    /// Windows accent / macOS appearance accent). `None` when the
    /// platform reports no override.
    pub accent_color: Option<Color>,

    pub high_contrast: bool,

    /// User text-size scaling. `1.0` is the system default; `0.85`,
    /// `1.3`, `2.0`, etc. are legitimate values on both iOS and
    /// Android (clamped to a sane floor at render time by callers).
    pub font_scale: f32,

    pub reduce_motion: bool,

    pub reduce_transparency: bool,

    pub bold_text: bool,

    pub invert_colors: bool,

    pub screen_reader_active: bool,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorScheme {
    Light,
    Dark,
    /// Platform follows system / time-of-day — some platforms expose
    /// the resolved value instead of the preference, in which case the
    /// publisher reports `Light` or `Dark` directly.
    #[default]
    Auto,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Color {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Color {
    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    #[must_use]
    pub const fn rgba(r: u8, g: u8, b: u8, a: u8) -> Self {
        Self { r, g, b, a }
    }
}

#[derive(Debug, Clone)]
pub struct ThemePreferenceClient {
    slot: Arc<LatestValueSlot>,
}

impl Plugin for ThemePreferenceClient {
    const PLUGIN_ID: &'static str = THEME_PREFERENCE_PLUGIN_ID;
}

impl ThemePreferenceClient {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        Ok(Self {
            slot: rt.early_events().latest_slot(THEME_PREFERENCE_CHANNEL),
        })
    }

    pub fn current(&self) -> Result<Option<ThemePreference>, IstmoError> {
        self.slot.peek().map_or(Ok(None), |b| decode(&b).map(Some))
    }

    #[must_use]
    pub fn updates(&self) -> ThemePreferenceStream {
        ThemePreferenceStream {
            rx: self.slot.subscribe(),
        }
    }
}

#[derive(Debug)]
pub struct ThemePreferenceStream {
    rx: Receiver<Vec<u8>>,
}

impl ThemePreferenceStream {
    pub fn recv(&self) -> Result<ThemePreference, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    pub async fn recv_async(&self) -> Result<ThemePreference, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<ThemePreference, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode(&b))
    }
}

fn decode(bytes: &[u8]) -> Result<ThemePreference, IstmoError> {
    let (v, _) = codec::decode::<ThemePreference>(bytes)?;
    Ok(v)
}
