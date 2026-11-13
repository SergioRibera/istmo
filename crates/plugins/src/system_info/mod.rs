//! System info — locale, time zone, formatting preferences and the
//! device wall clock.
//!
//! Three latest-value early-event streams back the surface so late
//! subscribers see the current value on attach:
//!
//! - [`SYSTEM_INFO_LOCALE_CHANNEL`] — [`Locale`] (BCP-47 + layout
//!   direction).
//! - [`SYSTEM_INFO_TIMEZONE_CHANNEL`] — [`TimeZone`] (IANA id + offset
//!   + DST).
//! - [`SYSTEM_INFO_FORMATS_CHANNEL`] — [`Formats`] (24h, first day of
//!   week, measurement, currency, separators, calendar).
//!
//! Native sides push updates when the user changes OS settings
//! (`Intent.ACTION_LOCALE_CHANGED` on Android,
//! `NSCurrentLocaleDidChangeNotification` on iOS/macOS, etc.).
//! Desktop targets (Linux / macOS / Windows) expose
//! [`SystemInfoPublisher`] — a pure-Rust sampler that attached
//! desktop apps can drive from their event loop; Web is stubbed and
//! expects an OS-level bridge.
//!
//! `now_utc_millis` is pure Rust (`SystemTime::now()`); every other
//! value needs the platform to publish.

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

use crate::early::{EarlyStream, LatestClient, LatestValue};

pub mod tables;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod publisher;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use publisher::SystemInfoPublisher;

/// Plugin id of the system info streams.
pub const SYSTEM_INFO_PLUGIN_ID: &str = "istmo.system_info";
/// Early-event channel of [`Locale`].
pub const SYSTEM_INFO_LOCALE_CHANNEL: &str = "istmo.system_info.locale";
/// Early-event channel of [`TimeZone`].
pub const SYSTEM_INFO_TIMEZONE_CHANNEL: &str = "istmo.system_info.timezone";
/// Early-event channel of [`Formats`].
pub const SYSTEM_INFO_FORMATS_CHANNEL: &str = "istmo.system_info.formats";
/// Early-event channel of the uptime in milliseconds (`u64`).
pub const SYSTEM_INFO_UPTIME_CHANNEL: &str = "istmo.system_info.uptime";

/// The user's language and region.
#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Locale {
    /// Canonical BCP-47 tag, e.g. `es-AR`, `zh-Hans-CN`.
    pub bcp47: String,
    /// Primary language subtag, lower-cased: `es`, `zh`.
    pub language: String,
    /// ISO 3166-1 alpha-2 or UN M.49 region, upper-cased: `AR`.
    pub region: Option<String>,
    /// ISO 15924 script, Title-cased: `Latn`, `Hans`.
    pub script: Option<String>,
    pub direction: LayoutDirection,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayoutDirection {
    #[default]
    Ltr,
    Rtl,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TimeZone {
    /// IANA id, e.g. `America/Argentina/Buenos_Aires`. Platforms that
    /// only expose a Windows id must map to IANA before publishing
    /// (ICU `ucal_getDefaultTimeZone` on Windows 10+ returns IANA).
    pub id: String,
    /// Current UTC offset in seconds. Negative west of UTC.
    pub offset_seconds: i32,
    /// Whether the current offset already reflects DST.
    pub is_dst: bool,
    /// Short abbreviation (`ART`, `PDT`). Best-effort — fall back to
    /// the IANA id when the platform cannot provide one.
    pub abbreviation: String,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Formats {
    pub uses_24h: bool,
    /// ISO weekday where Monday = 1, Sunday = 7.
    pub first_day_of_week: u8,
    pub measurement: MeasurementSystem,
    pub temperature_unit: TemperatureUnit,
    /// ISO 4217, when derivable. `None` for unknown regions.
    pub currency_code: Option<String>,
    pub decimal_separator: String,
    pub group_separator: String,
    pub calendar: CalendarSystem,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MeasurementSystem {
    #[default]
    Metric,
    UsCustomary,
    UkImperial,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[message(wincode = "::wincode")]
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum CalendarSystem {
    #[default]
    Gregorian,
    Buddhist,
    Japanese,
    Islamic,
    Hebrew,
    Persian,
    /// Any other CLDR calendar identifier reported by the platform.
    Other(String),
}

impl LatestValue for Locale {
    const PLUGIN_ID: &'static str = SYSTEM_INFO_PLUGIN_ID;
    const CHANNEL: &'static str = SYSTEM_INFO_LOCALE_CHANNEL;
}

impl LatestValue for TimeZone {
    const PLUGIN_ID: &'static str = SYSTEM_INFO_PLUGIN_ID;
    const CHANNEL: &'static str = SYSTEM_INFO_TIMEZONE_CHANNEL;
}

impl LatestValue for Formats {
    const PLUGIN_ID: &'static str = SYSTEM_INFO_PLUGIN_ID;
    const CHANNEL: &'static str = SYSTEM_INFO_FORMATS_CHANNEL;
}

/// Locales as they are published.
pub type LocaleStream = EarlyStream<Locale>;

/// Time zones as they are published.
pub type TimeZoneStream = EarlyStream<TimeZone>;

/// Formatting preferences as they are published.
pub type FormatsStream = EarlyStream<Formats>;

/// Reader for the locale, time zone, formats and uptime the platform
/// publishes.
#[derive(Debug, Clone)]
pub struct SystemInfoClient {
    locale: LatestClient<Locale>,
    timezone: LatestClient<TimeZone>,
    formats: LatestClient<Formats>,
    uptime: Arc<LatestValueSlot>,
}

impl Plugin for SystemInfoClient {
    const PLUGIN_ID: &'static str = SYSTEM_INFO_PLUGIN_ID;
}

impl SystemInfoClient {
    /// Client bound to the process-wide runtime.
    ///
    /// # Errors
    ///
    /// Fails when no global runtime is installed or the plugin was not
    /// declared.
    pub fn acquire() -> Result<Self, IstmoError> {
        Self::from_runtime(&Runtime::global()?)
    }

    /// Client bound to `rt`.
    ///
    /// # Errors
    ///
    /// [`IstmoError::PluginNotDeclared`] when enforcement is on and the
    /// plugin was not declared.
    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        Ok(Self {
            locale: LatestClient::from_runtime(rt)?,
            timezone: LatestClient::from_runtime(rt)?,
            formats: LatestClient::from_runtime(rt)?,
            uptime: rt.early_events().latest_slot(SYSTEM_INFO_UPTIME_CHANNEL),
        })
    }

    /// The retained locale, `None` before the first publish.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn current_locale(&self) -> Result<Option<Locale>, IstmoError> {
        self.locale.current()
    }

    /// The retained time zone, `None` before the first publish.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn current_timezone(&self) -> Result<Option<TimeZone>, IstmoError> {
        self.timezone.current()
    }

    /// The retained formats, `None` before the first publish.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn current_formats(&self) -> Result<Option<Formats>, IstmoError> {
        self.formats.current()
    }

    /// Subscribe to locale changes, starting with the retained value.
    #[must_use]
    pub fn locale_stream(&self) -> LocaleStream {
        self.locale.stream()
    }

    /// Subscribe to time zone changes, starting with the retained value.
    #[must_use]
    pub fn timezone_stream(&self) -> TimeZoneStream {
        self.timezone.stream()
    }

    /// Subscribe to formats changes, starting with the retained value.
    #[must_use]
    pub fn formats_stream(&self) -> FormatsStream {
        self.formats.stream()
    }

    /// Unix-epoch milliseconds. Pure Rust, no FFI.
    #[must_use]
    pub fn now_utc_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    /// Milliseconds since the device booted, as of the last publish.
    /// `None` when no publisher has supplied a value — desktops wire
    /// this through [`SystemInfoPublisher`], mobile through a native
    /// bridge.
    ///
    /// # Errors
    ///
    /// [`IstmoError::Codec`] when the retained payload does not decode.
    pub fn uptime_millis(&self) -> Result<Option<u64>, IstmoError> {
        self.uptime
            .peek()
            .map(|bytes| Ok(codec::decode::<u64>(&bytes)?.0))
            .transpose()
    }
}
