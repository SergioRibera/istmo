//! User-facing theme and accessibility preferences published by the
//! host platform.
//!
//! Native observers (`UITraitCollection` / `UIAccessibility` on Apple,
//! `AccessibilityManager` + `UIMode` on Android, `prefers-*` on Web,
//! `gsettings` on Linux, `SPI_GET*` on Windows) feed a single
//! [`ThemePreference`] snapshot through a latest-value slot so late
//! subscribers get the current state immediately.

use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

/// Plugin id of the theme preference stream.
pub const THEME_PREFERENCE_PLUGIN_ID: &str = "istmo.theme_preference";
/// Early-event channel the theme snapshot is published on.
pub const THEME_PREFERENCE_CHANNEL: &str = "istmo.theme_preference";

/// Appearance and accessibility preferences.
#[message(wincode = "::wincode")]
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

#[message(wincode = "::wincode")]
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

#[message(wincode = "::wincode")]
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

impl LatestValue for ThemePreference {
    const PLUGIN_ID: &'static str = THEME_PREFERENCE_PLUGIN_ID;
    const CHANNEL: &'static str = THEME_PREFERENCE_CHANNEL;
}

/// Reader for the theme snapshot the platform publishes.
pub type ThemePreferenceClient = LatestClient<ThemePreference>;

/// Theme snapshots as they are published.
pub type ThemePreferenceStream = EarlyStream<ThemePreference>;
