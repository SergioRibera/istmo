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

use flume::Receiver;
use istmo_core::early_events::LatestValueSlot;
use istmo_core::{IstmoError, Plugin, Runtime, codec};
use istmo_macros::message;

pub mod tables;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub mod publisher;

#[cfg(any(target_os = "linux", target_os = "macos", target_os = "windows"))]
pub use publisher::SystemInfoPublisher;

pub const SYSTEM_INFO_PLUGIN_ID: &str = "istmo.system_info";
pub const SYSTEM_INFO_LOCALE_CHANNEL: &str = "istmo.system_info.locale";
pub const SYSTEM_INFO_TIMEZONE_CHANNEL: &str = "istmo.system_info.timezone";
pub const SYSTEM_INFO_FORMATS_CHANNEL: &str = "istmo.system_info.formats";
pub const SYSTEM_INFO_UPTIME_CHANNEL: &str = "istmo.system_info.uptime";

#[message(bincode = "::bincode")]
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

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LayoutDirection {
    #[default]
    Ltr,
    Rtl,
}

#[message(bincode = "::bincode")]
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

#[message(bincode = "::bincode")]
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

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum MeasurementSystem {
    #[default]
    Metric,
    UsCustomary,
    UkImperial,
}

#[message(bincode = "::bincode")]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TemperatureUnit {
    #[default]
    Celsius,
    Fahrenheit,
}

#[message(bincode = "::bincode")]
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

#[derive(Debug, Clone)]
#[allow(clippy::struct_field_names)]
pub struct SystemInfo {
    locale_slot: Arc<LatestValueSlot>,
    timezone_slot: Arc<LatestValueSlot>,
    formats_slot: Arc<LatestValueSlot>,
    uptime_slot: Arc<LatestValueSlot>,
}

impl Plugin for SystemInfo {
    const PLUGIN_ID: &'static str = SYSTEM_INFO_PLUGIN_ID;
}

impl SystemInfo {
    pub fn acquire() -> Result<Self, IstmoError> {
        let rt = Runtime::global()?;
        Self::from_runtime(&rt)
    }

    pub fn from_runtime(rt: &Arc<Runtime>) -> Result<Self, IstmoError> {
        rt.check_declared(Self::PLUGIN_ID)?;
        let early = rt.early_events();
        Ok(Self {
            locale_slot: early.latest_slot(SYSTEM_INFO_LOCALE_CHANNEL),
            timezone_slot: early.latest_slot(SYSTEM_INFO_TIMEZONE_CHANNEL),
            formats_slot: early.latest_slot(SYSTEM_INFO_FORMATS_CHANNEL),
            uptime_slot: early.latest_slot(SYSTEM_INFO_UPTIME_CHANNEL),
        })
    }

    pub fn current_locale(&self) -> Result<Option<Locale>, IstmoError> {
        peek_locale(&self.locale_slot)
    }

    pub fn current_timezone(&self) -> Result<Option<TimeZone>, IstmoError> {
        peek_timezone(&self.timezone_slot)
    }

    pub fn current_formats(&self) -> Result<Option<Formats>, IstmoError> {
        peek_formats(&self.formats_slot)
    }

    #[must_use]
    pub fn locale_updates(&self) -> LocaleStream {
        LocaleStream {
            rx: self.locale_slot.subscribe(),
        }
    }

    #[must_use]
    pub fn timezone_updates(&self) -> TimeZoneStream {
        TimeZoneStream {
            rx: self.timezone_slot.subscribe(),
        }
    }

    #[must_use]
    pub fn formats_updates(&self) -> FormatsStream {
        FormatsStream {
            rx: self.formats_slot.subscribe(),
        }
    }

    /// Unix-epoch milliseconds. Pure Rust, no FFI.
    #[must_use]
    pub fn now_utc_millis(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
    }

    /// Milliseconds since the device booted. Returns `None` when no
    /// publisher has supplied a value — platforms wire this via
    /// [`UptimePublisher`](publisher::UptimePublisher) on desktop or a
    /// native bridge on mobile.
    pub fn uptime_millis(&self) -> Result<Option<u64>, IstmoError> {
        self.uptime_slot.peek().map_or(Ok(None), |b| {
            let (v, _) = codec::decode::<u64>(&b)?;
            Ok(Some(v))
        })
    }
}


#[derive(Debug)]
pub struct LocaleStream {
    rx: Receiver<Vec<u8>>,
}

impl LocaleStream {
    pub fn recv(&self) -> Result<Locale, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode_locale(&bytes)
    }

    pub async fn recv_async(&self) -> Result<Locale, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode_locale(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<Locale, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode_locale(&b))
    }
}

#[derive(Debug)]
pub struct TimeZoneStream {
    rx: Receiver<Vec<u8>>,
}

impl TimeZoneStream {
    pub fn recv(&self) -> Result<TimeZone, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode_timezone(&bytes)
    }

    pub async fn recv_async(&self) -> Result<TimeZone, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode_timezone(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<TimeZone, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode_timezone(&b))
    }
}

#[derive(Debug)]
pub struct FormatsStream {
    rx: Receiver<Vec<u8>>,
}

impl FormatsStream {
    pub fn recv(&self) -> Result<Formats, IstmoError> {
        let bytes = self.rx.recv().map_err(|_| IstmoError::ChannelClosed)?;
        decode_formats(&bytes)
    }

    pub async fn recv_async(&self) -> Result<Formats, IstmoError> {
        let bytes = self
            .rx
            .recv_async()
            .await
            .map_err(|_| IstmoError::ChannelClosed)?;
        decode_formats(&bytes)
    }

    #[must_use]
    pub fn try_recv(&self) -> Option<Result<Formats, IstmoError>> {
        self.rx.try_recv().ok().map(|b| decode_formats(&b))
    }
}

fn peek_locale(slot: &LatestValueSlot) -> Result<Option<Locale>, IstmoError> {
    slot.peek()
        .map_or(Ok(None), |b| decode_locale(&b).map(Some))
}

fn peek_timezone(slot: &LatestValueSlot) -> Result<Option<TimeZone>, IstmoError> {
    slot.peek()
        .map_or(Ok(None), |b| decode_timezone(&b).map(Some))
}

fn peek_formats(slot: &LatestValueSlot) -> Result<Option<Formats>, IstmoError> {
    slot.peek()
        .map_or(Ok(None), |b| decode_formats(&b).map(Some))
}

fn decode_locale(bytes: &[u8]) -> Result<Locale, IstmoError> {
    let (v, _) = codec::decode::<Locale>(bytes)?;
    Ok(v)
}

fn decode_timezone(bytes: &[u8]) -> Result<TimeZone, IstmoError> {
    let (v, _) = codec::decode::<TimeZone>(bytes)?;
    Ok(v)
}

fn decode_formats(bytes: &[u8]) -> Result<Formats, IstmoError> {
    let (v, _) = codec::decode::<Formats>(bytes)?;
    Ok(v)
}
