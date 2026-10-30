//! Pure-Rust desktop sampler for [`super::SystemInfo`].
//!
//! Call [`SystemInfoPublisher::install`] once at startup and tick from
//! an event loop (or on OS change notifications). Linux reads env +
//! `/etc/localtime`; macOS calls Foundation via `objc2`; Windows and
//! WebAssembly are stubbed and expect an OS-level bridge from the
//! surrounding runtime (same shape as `SafeAreaPublisher`'s Kotlin /
//! Swift side).
//!
//! Values are coarse by design — accurate wall-clock offset / DST on
//! Linux requires a tz database crate (`chrono-tz`, `time-tz`); this
//! sampler reports the IANA id and leaves offset at `0, is_dst =
//! false` so downstream can resolve precisely with the tz library of
//! its choice.

use std::sync::{Arc, Weak};

use istmo_core::{IstmoError, Runtime, codec};

use super::{
    Formats, Locale, SYSTEM_INFO_FORMATS_CHANNEL, SYSTEM_INFO_LOCALE_CHANNEL,
    SYSTEM_INFO_PLUGIN_ID, SYSTEM_INFO_TIMEZONE_CHANNEL, TimeZone,
};

#[derive(Debug)]
pub struct SystemInfoPublisher {
    runtime: Weak<Runtime>,
    last_locale: Option<Locale>,
    last_timezone: Option<TimeZone>,
    last_formats: Option<Formats>,
}

impl SystemInfoPublisher {
    pub fn install(runtime: &Arc<Runtime>) -> Result<Self, IstmoError> {
        runtime.check_declared(SYSTEM_INFO_PLUGIN_ID)?;
        Ok(Self {
            runtime: Arc::downgrade(runtime),
            last_locale: None,
            last_timezone: None,
            last_formats: None,
        })
    }

    /// Sample the host and publish any changed values. Safe to call on
    /// every frame — publishes only when a value actually changed.
    pub fn tick(&mut self) {
        let Some(runtime) = self.runtime.upgrade() else {
            return;
        };
        let Some(sample) = sample() else {
            return;
        };

        if self.last_locale.as_ref() != Some(&sample.locale) {
            match codec::encode(&sample.locale) {
                Ok(bytes) => {
                    runtime.publish_early_latest(SYSTEM_INFO_LOCALE_CHANNEL, bytes);
                    self.last_locale = Some(sample.locale);
                }
                Err(err) => tracing::warn!(target = "istmo.system_info", ?err, "locale encode"),
            }
        }

        if self.last_timezone.as_ref() != Some(&sample.timezone) {
            match codec::encode(&sample.timezone) {
                Ok(bytes) => {
                    runtime.publish_early_latest(SYSTEM_INFO_TIMEZONE_CHANNEL, bytes);
                    self.last_timezone = Some(sample.timezone);
                }
                Err(err) => tracing::warn!(target = "istmo.system_info", ?err, "tz encode"),
            }
        }

        if self.last_formats.as_ref() != Some(&sample.formats) {
            match codec::encode(&sample.formats) {
                Ok(bytes) => {
                    runtime.publish_early_latest(SYSTEM_INFO_FORMATS_CHANNEL, bytes);
                    self.last_formats = Some(sample.formats);
                }
                Err(err) => tracing::warn!(target = "istmo.system_info", ?err, "formats encode"),
            }
        }
    }
}

#[derive(Debug)]
struct Sample {
    locale: Locale,
    timezone: TimeZone,
    formats: Formats,
}

#[allow(clippy::unnecessary_wraps)]
fn sample() -> Option<Sample> {
    #[cfg(target_os = "linux")]
    {
        Some(linux::sample())
    }
    #[cfg(any(target_os = "macos", target_os = "ios"))]
    {
        apple::sample()
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "ios")))]
    {
        // Windows / WASM / BSDs: no pure-Rust path yet. Wire a native
        // bridge (same pattern as Android / iOS plugin backends) and
        // publish directly via `Runtime::publish_early_latest`.
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use std::fs;

    use super::super::{
        CalendarSystem, Formats, Locale, TimeZone,
        tables::{
            canonicalize_tag, currency_for_region, direction_from_language,
            measurement_for_region, parse_bcp47, temperature_for_region,
        },
    };
    use super::Sample;

    pub(super) fn sample() -> Sample {
        let raw_locale = read_env_locale();
        let (language, script, region) = parse_bcp47(&raw_locale);
        let direction = direction_from_language(&language);
        let bcp47 = canonicalize_tag(&language, script.as_deref(), region.as_deref());

        let locale = Locale {
            bcp47,
            language: language.clone(),
            region: region.clone(),
            script,
            direction,
        };

        let timezone = TimeZone {
            id: read_timezone_id().unwrap_or_else(|| "UTC".into()),
            offset_seconds: 0,
            is_dst: false,
            abbreviation: String::new(),
        };

        let (decimal_separator, group_separator) = separators_for(&language);
        let formats = Formats {
            uses_24h: uses_24h_for(&language, region.as_deref()),
            first_day_of_week: first_day_of_week_for(region.as_deref()),
            measurement: measurement_for_region(region.as_deref()),
            temperature_unit: temperature_for_region(region.as_deref()),
            currency_code: currency_for_region(region.as_deref()).map(str::to_owned),
            decimal_separator,
            group_separator,
            calendar: CalendarSystem::Gregorian,
        };

        Sample { locale, timezone, formats }
    }

    fn read_env_locale() -> String {
        for key in ["LC_ALL", "LC_MESSAGES", "LANG", "LANGUAGE"] {
            if let Ok(v) = std::env::var(key) {
                let head = v.split(':').next().unwrap_or(&v);
                if !head.is_empty() && head != "C" && head != "POSIX" {
                    return head.to_string();
                }
            }
        }
        "en-US".to_string()
    }

    const ZONEINFO_MARKER: &str = "zoneinfo/";

    fn read_timezone_id() -> Option<String> {
        if let Ok(tz) = std::env::var("TZ") {
            if !tz.is_empty() {
                return Some(tz.trim_start_matches(':').to_string());
            }
        }
        let link = fs::read_link("/etc/localtime").ok()?;
        let s = link.to_str()?;
        s.find(ZONEINFO_MARKER)
            .map(|i| s[i + ZONEINFO_MARKER.len()..].to_string())
    }

    /// Heuristic — most Latin / Germanic / Slavic locales default to
    /// 24h; US-English + a few derivatives default to 12h.
    fn uses_24h_for(language: &str, region: Option<&str>) -> bool {
        !matches!(
            (language, region),
            ("en", Some("US" | "AU" | "CA" | "NZ" | "PH")) | ("es", Some("MX" | "US"))
        )
    }

    /// ISO weekday: Monday = 1, Sunday = 7. Most of Europe + ISO 8601
    /// uses Monday; US / Canada / most Latin America / parts of Asia
    /// use Sunday; Middle East uses Saturday.
    fn first_day_of_week_for(region: Option<&str>) -> u8 {
        match region {
            Some("US" | "CA" | "MX" | "BR" | "AR" | "CL" | "CO" | "PE" | "VE" | "JP" | "KR"
            | "CN" | "TW" | "HK" | "IL" | "ZA" | "PH" | "TH") => 7,
            Some("AF" | "BH" | "DZ" | "EG" | "IQ" | "JO" | "KW" | "LY" | "OM" | "QA" | "SA"
            | "SD" | "SY" | "YE") => 6,
            _ => 1,
        }
    }

    fn separators_for(language: &str) -> (String, String) {
        match language {
            "en" | "zh" | "ja" | "ko" | "th" => (".".into(), ",".into()),
            _ => (",".into(), ".".into()),
        }
    }
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
#[allow(unsafe_code)]
mod apple {
    use objc2::msg_send;
    use objc2::runtime::{AnyClass, AnyObject};

    use super::super::{
        CalendarSystem, Formats, Locale, MeasurementSystem, TimeZone,
        tables::{
            canonicalize_tag, currency_for_region, direction_from_language,
            measurement_for_region, parse_bcp47, temperature_for_region,
        },
    };
    use super::Sample;

    pub(super) fn sample() -> Option<Sample> {
        // SAFETY: `NSLocale`, `NSTimeZone`, `NSCalendar` singletons
        // returned by `autoupdating*` are retained by Foundation for
        // the process lifetime. Every message send is null-checked;
        // NSString → Rust `String` goes through `UTF8String` whose
        // buffer is valid until the next autorelease pool drains —
        // we copy into an owned `String` immediately.
        let locale_obj = unsafe { autoupdating_locale() }?;
        let bcp47 = unsafe { locale_identifier(locale_obj) }?;
        let (language, script, region) = parse_bcp47(&bcp47);
        let canonical = canonicalize_tag(&language, script.as_deref(), region.as_deref());
        let direction = direction_from_language(&language);

        let uses_metric: bool = unsafe { msg_send![locale_obj, usesMetricSystem] };
        let measurement = if uses_metric {
            MeasurementSystem::Metric
        } else {
            measurement_for_region(region.as_deref())
        };

        let decimal_separator =
            unsafe { decimal_separator(locale_obj) }.unwrap_or_else(|| ".".into());
        let group_separator =
            unsafe { grouping_separator(locale_obj) }.unwrap_or_else(|| ",".into());
        let currency = unsafe { currency_code(locale_obj) }
            .or_else(|| currency_for_region(region.as_deref()).map(str::to_owned));

        let tz_obj = unsafe { local_time_zone() }?;
        let tz_id = unsafe { tz_name(tz_obj) }.unwrap_or_else(|| "UTC".into());
        let offset_seconds: isize = unsafe { msg_send![tz_obj, secondsFromGMT] };
        let is_dst: bool = unsafe { msg_send![tz_obj, isDaylightSavingTime] };
        let abbreviation = unsafe { tz_abbreviation(tz_obj) }.unwrap_or_default();

        let (first_day_of_week, calendar_system) = unsafe { autoupdating_calendar() }
            .map(|c| {
                let fw: isize = unsafe { msg_send![c, firstWeekday] };
                let id = unsafe { calendar_identifier(c) }
                    .unwrap_or_else(|| "gregorian".into());
                (apple_weekday_to_iso(fw), calendar_from_id(&id))
            })
            .unwrap_or((1, CalendarSystem::Gregorian));

        let locale = Locale {
            bcp47: canonical,
            language: language.clone(),
            region: region.clone(),
            script,
            direction,
        };

        let timezone = TimeZone {
            id: tz_id,
            offset_seconds: i32::try_from(offset_seconds).unwrap_or(0),
            is_dst,
            abbreviation,
        };

        let formats = Formats {
            uses_24h: !locale_has_am_pm(&language, region.as_deref()),
            first_day_of_week,
            measurement,
            temperature_unit: temperature_for_region(region.as_deref()),
            currency_code: currency,
            decimal_separator,
            group_separator,
            calendar: calendar_system,
        };

        Some(Sample { locale, timezone, formats })
    }

    unsafe fn autoupdating_locale() -> Option<*mut AnyObject> {
        let cls = AnyClass::get(c"NSLocale")?;
        let obj: *mut AnyObject = unsafe { msg_send![cls, autoupdatingCurrentLocale] };
        (!obj.is_null()).then_some(obj)
    }

    unsafe fn local_time_zone() -> Option<*mut AnyObject> {
        let cls = AnyClass::get(c"NSTimeZone")?;
        let obj: *mut AnyObject = unsafe { msg_send![cls, localTimeZone] };
        (!obj.is_null()).then_some(obj)
    }

    unsafe fn autoupdating_calendar() -> Option<*mut AnyObject> {
        let cls = AnyClass::get(c"NSCalendar")?;
        let obj: *mut AnyObject = unsafe { msg_send![cls, autoupdatingCurrentCalendar] };
        (!obj.is_null()).then_some(obj)
    }

    unsafe fn locale_identifier(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, localeIdentifier] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn decimal_separator(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, decimalSeparator] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn grouping_separator(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, groupingSeparator] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn currency_code(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, currencyCode] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn tz_name(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, name] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn tz_abbreviation(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, abbreviation] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn calendar_identifier(obj: *mut AnyObject) -> Option<String> {
        let ns: *mut AnyObject = unsafe { msg_send![obj, calendarIdentifier] };
        unsafe { ns_string_to_rust(ns) }
    }

    unsafe fn ns_string_to_rust(ns: *mut AnyObject) -> Option<String> {
        if ns.is_null() {
            return None;
        }
        let bytes: *const i8 = unsafe { msg_send![ns, UTF8String] };
        if bytes.is_null() {
            return None;
        }
        let c_str = unsafe { core::ffi::CStr::from_ptr(bytes) };
        c_str.to_str().ok().map(ToOwned::to_owned)
    }

    /// Apple weekday: 1 = Sunday … 7 = Saturday. ISO: 1 = Monday … 7 =
    /// Sunday.
    fn apple_weekday_to_iso(apple: isize) -> u8 {
        match apple {
            1 => 7,
            n if (2..=7).contains(&n) => u8::try_from(n - 1).unwrap_or(1),
            _ => 1,
        }
    }

    fn calendar_from_id(id: &str) -> CalendarSystem {
        match id {
            "gregorian" => CalendarSystem::Gregorian,
            "buddhist" => CalendarSystem::Buddhist,
            "japanese" => CalendarSystem::Japanese,
            "islamic" | "islamic-civil" | "islamic-umalqura" | "islamic-tbla"
            | "islamic-rgsa" => CalendarSystem::Islamic,
            "hebrew" => CalendarSystem::Hebrew,
            "persian" => CalendarSystem::Persian,
            other => CalendarSystem::Other(other.to_owned()),
        }
    }

    fn locale_has_am_pm(language: &str, region: Option<&str>) -> bool {
        matches!(
            (language, region),
            ("en", Some("US" | "AU" | "CA" | "NZ" | "PH"))
                | ("es", Some("MX" | "US"))
        )
    }
}
