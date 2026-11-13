use istmo_core::{Runtime, codec};
use istmo_plugins::{
    CalendarSystem, Formats, LayoutDirection, Locale, MeasurementSystem,
    SYSTEM_INFO_FORMATS_CHANNEL, SYSTEM_INFO_LOCALE_CHANNEL, SYSTEM_INFO_TIMEZONE_CHANNEL,
    SystemInfoClient, TemperatureUnit, TimeZone,
};

fn sample_locale() -> Locale {
    Locale {
        bcp47: "es-AR".into(),
        language: "es".into(),
        region: Some("AR".into()),
        script: None,
        direction: LayoutDirection::Ltr,
    }
}

fn sample_timezone() -> TimeZone {
    TimeZone {
        id: "America/Argentina/Buenos_Aires".into(),
        offset_seconds: -3 * 3600,
        is_dst: false,
        abbreviation: "ART".into(),
    }
}

fn sample_formats() -> Formats {
    Formats {
        uses_24h: true,
        first_day_of_week: 1,
        measurement: MeasurementSystem::Metric,
        temperature_unit: TemperatureUnit::Celsius,
        currency_code: Some("ARS".into()),
        decimal_separator: ",".into(),
        group_separator: ".".into(),
        calendar: CalendarSystem::Gregorian,
    }
}

fn publish<T>(rt: &std::sync::Arc<Runtime>, channel: &str, value: &T)
where
    T: codec::Encode,
{
    let bytes = codec::encode(value).expect("encode");
    rt.publish_early_latest(channel, bytes);
}

#[test]
fn current_values_are_none_before_any_publish() {
    let init = Runtime::mock();
    let plugin = SystemInfoClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.current_locale().unwrap().is_none());
    assert!(plugin.current_timezone().unwrap().is_none());
    assert!(plugin.current_formats().unwrap().is_none());
}

#[test]
fn late_subscriber_receives_retained_locale_first() {
    let init = Runtime::mock();
    let rt = init.runtime;

    publish(&rt, SYSTEM_INFO_LOCALE_CHANNEL, &sample_locale());

    let plugin = SystemInfoClient::from_runtime(&rt).expect("declared");
    assert_eq!(plugin.current_locale().unwrap(), Some(sample_locale()));

    let stream = plugin.locale_stream();
    assert_eq!(stream.recv().unwrap(), sample_locale());
}

#[test]
fn timezone_stream_delivers_updates() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = SystemInfoClient::from_runtime(&rt).expect("declared");
    let stream = plugin.timezone_stream();

    publish(&rt, SYSTEM_INFO_TIMEZONE_CHANNEL, &sample_timezone());
    assert_eq!(stream.recv().unwrap(), sample_timezone());

    let mut next = sample_timezone();
    next.offset_seconds = -2 * 3600;
    next.is_dst = true;
    next.abbreviation = "ARST".into();
    publish(&rt, SYSTEM_INFO_TIMEZONE_CHANNEL, &next);
    assert_eq!(stream.recv().unwrap(), next);
}

#[test]
fn formats_stream_delivers_updates() {
    let init = Runtime::mock();
    let rt = init.runtime;
    let plugin = SystemInfoClient::from_runtime(&rt).expect("declared");
    let stream = plugin.formats_stream();

    publish(&rt, SYSTEM_INFO_FORMATS_CHANNEL, &sample_formats());
    assert_eq!(stream.recv().unwrap(), sample_formats());
}

#[test]
fn now_utc_millis_is_positive_and_recent() {
    let init = Runtime::mock();
    let plugin = SystemInfoClient::from_runtime(&init.runtime).expect("declared");
    let now = plugin.now_utc_millis();
    // 2024-01-01 UTC in ms as a sanity floor.
    assert!(now > 1_704_067_200_000);
}

#[test]
fn try_recv_returns_none_when_empty() {
    let init = Runtime::mock();
    let plugin = SystemInfoClient::from_runtime(&init.runtime).expect("declared");
    assert!(plugin.locale_stream().try_recv().is_none());
    assert!(plugin.timezone_stream().try_recv().is_none());
    assert!(plugin.formats_stream().try_recv().is_none());
}

#[test]
#[cfg(any(
    target_os = "linux",
    target_os = "android",
    target_os = "macos",
    target_os = "windows"
))]
fn uptime_is_read_from_the_os_clock_without_a_publish() {
    let init = Runtime::mock();
    let plugin = SystemInfoClient::from_runtime(&init.runtime).expect("declared");
    let first = plugin
        .uptime_millis()
        .expect("uptime readable on this host");
    let second = plugin
        .uptime_millis()
        .expect("uptime readable on this host");
    assert!(first > 0);
    assert!(
        second >= first,
        "uptime went backwards: {first} -> {second}"
    );
}
