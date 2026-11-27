//! Runs `tests/schedule_vectors.tsv` against `CalendarRule`. The Kotlin
//! and Swift ports run the same file; the column format is documented at
//! its top.

use istmo_notifications::{CalendarRule, Weekday};
use jiff::Timestamp;
use jiff::tz::TimeZone;

fn instant(text: &str) -> i64 {
    text.parse::<Timestamp>()
        .unwrap_or_else(|err| panic!("bad instant {text:?}: {err}"))
        .as_millisecond()
}

fn optional(text: &str) -> Option<&str> {
    (text != "-").then_some(text)
}

fn weekday(text: &str) -> Weekday {
    match text {
        "mon" => Weekday::Mon,
        "tue" => Weekday::Tue,
        "wed" => Weekday::Wed,
        "thu" => Weekday::Thu,
        "fri" => Weekday::Fri,
        "sat" => Weekday::Sat,
        "sun" => Weekday::Sun,
        other => panic!("bad weekday {other:?}"),
    }
}

fn format(ms: &[i64]) -> String {
    if ms.is_empty() {
        return "none".to_owned();
    }
    ms.iter()
        .map(|ms| Timestamp::from_millisecond(*ms).unwrap().to_string())
        .collect::<Vec<_>>()
        .join(",")
}

#[test]
fn every_vector_matches() {
    let table = include_str!("schedule_vectors.tsv");
    let mut rows = 0;
    let mut failures = Vec::new();
    for line in table.lines() {
        if line.starts_with('#') || line.trim().is_empty() || line.starts_with("name\t") {
            continue;
        }
        let cols: Vec<&str> = line.split('\t').collect();
        let [
            name,
            zone,
            local,
            hour,
            minute,
            weekdays,
            day,
            month,
            until,
            after,
            count,
            expected,
        ] = cols[..]
        else {
            panic!("row with {} columns: {line:?}", cols.len());
        };
        let rule = CalendarRule {
            hour: hour.parse().unwrap(),
            minute: minute.parse().unwrap(),
            weekdays: optional(weekdays)
                .map(|list| list.split(',').map(weekday).collect())
                .unwrap_or_default(),
            day_of_month: optional(day).map(|d| d.parse().unwrap()),
            month: optional(month).map(|m| m.parse().unwrap()),
            time_zone: optional(zone).map(str::to_owned),
            until_ms: optional(until).map(instant),
        };
        let local =
            TimeZone::get(local).unwrap_or_else(|err| panic!("{name}: zone {local}: {err}"));
        let got = rule.next_occurrences_in(instant(after), count.parse().unwrap(), &local);
        let got = format(&got);
        if got != expected {
            failures.push(format!("{name}: expected {expected}, got {got}"));
        }
        rows += 1;
    }
    assert!(rows >= 30, "only {rows} vectors read");
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
