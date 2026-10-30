//! Hand-rolled CLDR-ish lookup tables and locale derivations.
//!
//! Covers the slice of locale data that platforms commonly do not
//! expose directly (layout direction, measurement system, currency,
//! canonical BCP-47 parsing). Intentionally no `icu4x` dependency —
//! swap in a richer table or feature-gated `icu` when a real consumer
//! outgrows these.

use super::{LayoutDirection, MeasurementSystem, TemperatureUnit};

const RTL_LANGUAGES: &[&str] = &[
    "ar", "arc", "ckb", "dv", "fa", "ha", "he", "khw", "ks", "ps", "sd", "ug", "ur", "yi",
];

const US_CUSTOMARY_REGIONS: &[&str] = &["US", "LR", "MM"];
const UK_IMPERIAL_REGIONS: &[&str] = &["GB"];

const FAHRENHEIT_REGIONS: &[&str] = &[
    "US", "BS", "BZ", "KY", "LR", "PW", "FM", "MH", "PR", "GU", "VI", "AS", "MP",
];

#[must_use]
pub fn direction_from_language(language: &str) -> LayoutDirection {
    if RTL_LANGUAGES.contains(&language) {
        LayoutDirection::Rtl
    } else {
        LayoutDirection::Ltr
    }
}

#[must_use]
pub fn measurement_for_region(region: Option<&str>) -> MeasurementSystem {
    match region {
        Some(r) if US_CUSTOMARY_REGIONS.contains(&r) => MeasurementSystem::UsCustomary,
        Some(r) if UK_IMPERIAL_REGIONS.contains(&r) => MeasurementSystem::UkImperial,
        _ => MeasurementSystem::Metric,
    }
}

#[must_use]
pub fn temperature_for_region(region: Option<&str>) -> TemperatureUnit {
    match region {
        Some(r) if FAHRENHEIT_REGIONS.contains(&r) => TemperatureUnit::Fahrenheit,
        _ => TemperatureUnit::Celsius,
    }
}

/// ISO 3166-1 alpha-2 region → ISO 4217 currency.
///
/// Covers major users; unknown regions return `None`. Downstream code
/// that needs full CLDR coverage should carry its own mapping (or add
/// an `icu` feature later).
#[must_use]
pub fn currency_for_region(region: Option<&str>) -> Option<&'static str> {
    let code = match region? {
        "AR" => "ARS",
        "AT" | "BE" | "CY" | "DE" | "EE" | "ES" | "FI" | "FR" | "GR" | "HR" | "IE" | "IT"
        | "LT" | "LU" | "LV" | "MT" | "NL" | "PT" | "SI" | "SK" | "AD" | "MC" | "SM" | "VA"
        | "XK" | "ME" => "EUR",
        "AU" => "AUD",
        "BO" => "BOB",
        "BR" => "BRL",
        "CA" => "CAD",
        "CH" => "CHF",
        "CL" => "CLP",
        "CN" => "CNY",
        "CO" => "COP",
        "CR" => "CRC",
        "CU" => "CUP",
        "CZ" => "CZK",
        "DK" => "DKK",
        "DO" => "DOP",
        "EG" => "EGP",
        "GB" => "GBP",
        "GT" => "GTQ",
        "HK" => "HKD",
        "HN" => "HNL",
        "HU" => "HUF",
        "ID" => "IDR",
        "IL" => "ILS",
        "IN" => "INR",
        "IS" => "ISK",
        "JP" => "JPY",
        "KR" => "KRW",
        "MA" => "MAD",
        "MX" => "MXN",
        "MY" => "MYR",
        "NG" => "NGN",
        "NI" => "NIO",
        "NO" => "NOK",
        "NZ" => "NZD",
        "PE" => "PEN",
        "PH" => "PHP",
        "PK" => "PKR",
        "PL" => "PLN",
        "PY" => "PYG",
        "RO" => "RON",
        "RU" => "RUB",
        "SA" => "SAR",
        "SE" => "SEK",
        "SG" => "SGD",
        "TH" => "THB",
        "TR" => "TRY",
        "TW" => "TWD",
        "UA" => "UAH",
        "US" | "PR" | "GU" | "VI" | "AS" | "MP" | "EC" | "SV" | "PA" | "TL" | "ZW" => "USD",
        "UY" => "UYU",
        "VE" => "VES",
        "VN" => "VND",
        "ZA" => "ZAR",
        _ => return None,
    };
    Some(code)
}

/// Parse a BCP-47 or POSIX-style tag (`en`, `en-US`, `en_US`,
/// `zh-Hans-CN`, `es_AR.UTF-8`) into `(language, script?, region?)`.
///
/// Language is lower-cased, script Title-case, region upper-case.
/// Leading language subtag is normalised; everything after `.` or `@`
/// is dropped (POSIX locale encoding / modifier).
#[must_use]
pub fn parse_bcp47(tag: &str) -> (String, Option<String>, Option<String>) {
    let head = tag.split(['.', '@']).next().unwrap_or(tag);
    let mut parts = head.split(['-', '_']).filter(|p| !p.is_empty());
    let language = parts.next().unwrap_or("").to_ascii_lowercase();
    let mut script = None;
    let mut region = None;
    for part in parts {
        if script.is_none()
            && part.len() == 4
            && part.chars().all(|c| c.is_ascii_alphabetic())
        {
            let mut s = String::with_capacity(4);
            for (i, c) in part.chars().enumerate() {
                if i == 0 {
                    s.push(c.to_ascii_uppercase());
                } else {
                    s.push(c.to_ascii_lowercase());
                }
            }
            script = Some(s);
        } else if region.is_none()
            && part.len() == 2
            && part.chars().all(|c| c.is_ascii_alphabetic())
        {
            region = Some(part.to_ascii_uppercase());
        } else if region.is_none()
            && part.len() == 3
            && part.chars().all(|c| c.is_ascii_digit())
        {
            region = Some(part.to_string());
        }
    }
    (language, script, region)
}

#[must_use]
pub fn canonicalize_tag(lang: &str, script: Option<&str>, region: Option<&str>) -> String {
    let cap = lang.len() + script.map_or(0, |s| s.len() + 1) + region.map_or(0, |r| r.len() + 1);
    let mut out = String::with_capacity(cap);
    out.push_str(lang);
    if let Some(s) = script {
        out.push('-');
        out.push_str(s);
    }
    if let Some(r) = region {
        out.push('-');
        out.push_str(r);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rtl_detection() {
        assert_eq!(direction_from_language("ar"), LayoutDirection::Rtl);
        assert_eq!(direction_from_language("he"), LayoutDirection::Rtl);
        assert_eq!(direction_from_language("en"), LayoutDirection::Ltr);
        assert_eq!(direction_from_language("es"), LayoutDirection::Ltr);
    }

    #[test]
    fn measurement_derivation() {
        assert_eq!(measurement_for_region(Some("US")), MeasurementSystem::UsCustomary);
        assert_eq!(measurement_for_region(Some("GB")), MeasurementSystem::UkImperial);
        assert_eq!(measurement_for_region(Some("AR")), MeasurementSystem::Metric);
        assert_eq!(measurement_for_region(None), MeasurementSystem::Metric);
    }

    #[test]
    fn temperature_derivation() {
        assert_eq!(temperature_for_region(Some("US")), TemperatureUnit::Fahrenheit);
        assert_eq!(temperature_for_region(Some("GB")), TemperatureUnit::Celsius);
        assert_eq!(temperature_for_region(None), TemperatureUnit::Celsius);
    }

    #[test]
    fn currency_derivation() {
        assert_eq!(currency_for_region(Some("AR")), Some("ARS"));
        assert_eq!(currency_for_region(Some("DE")), Some("EUR"));
        assert_eq!(currency_for_region(Some("US")), Some("USD"));
        assert_eq!(currency_for_region(Some("ZZ")), None);
        assert_eq!(currency_for_region(None), None);
    }

    #[test]
    fn parse_posix_locale() {
        assert_eq!(
            parse_bcp47("es_AR.UTF-8"),
            ("es".into(), None, Some("AR".into()))
        );
        assert_eq!(
            parse_bcp47("en_US@euro"),
            ("en".into(), None, Some("US".into()))
        );
    }

    #[test]
    fn parse_bcp47_script_and_region() {
        assert_eq!(
            parse_bcp47("zh-Hans-CN"),
            ("zh".into(), Some("Hans".into()), Some("CN".into()))
        );
        assert_eq!(
            parse_bcp47("sr-Latn"),
            ("sr".into(), Some("Latn".into()), None)
        );
    }

    #[test]
    fn parse_language_only() {
        assert_eq!(parse_bcp47("fr"), ("fr".into(), None, None));
    }

    #[test]
    fn canonicalize_round_trip() {
        let (lang, script, region) = parse_bcp47("zh_Hans_CN");
        assert_eq!(
            canonicalize_tag(&lang, script.as_deref(), region.as_deref()),
            "zh-Hans-CN"
        );
    }
}
