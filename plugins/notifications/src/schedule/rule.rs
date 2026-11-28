//! [`CalendarRule`] math with `jiff`.
//!
//! The rule is a wall-clock time on the days that match every set field
//! (`weekdays`, `day_of_month`, `month`). Resolution of a wall time to
//! an instant follows RFC 5545 / Temporal "compatible":
//!
//! * a time in a DST gap moves forward by the gap (`02:30` on a
//!   spring-forward day of a one-hour gap is `03:30` local);
//! * a time in a DST overlap is the earlier instant, and fires once;
//! * a day that does not match is skipped: `day_of_month = 31` skips
//!   30-day months, `29` in February skips non-leap years (Apple's
//!   `UNCalendarNotificationTrigger` matches the same way).
//!
//! The Kotlin port (`native/android/CalendarRule.kt`) and anything else
//! that evaluates rules must agree with `tests/schedule_vectors.tsv`.

use jiff::Timestamp;
use jiff::civil::{Date, Weekday as CivilWeekday};
use jiff::tz::TimeZone;

use crate::{CalendarRule, Weekday};

/// Years searched for a match: the Gregorian calendar repeats every 400
/// years, so a rule with no match in 400 years never matches.
const SEARCH_YEARS: i32 = 401;

const fn civil(day: Weekday) -> CivilWeekday {
    match day {
        Weekday::Mon => CivilWeekday::Monday,
        Weekday::Tue => CivilWeekday::Tuesday,
        Weekday::Wed => CivilWeekday::Wednesday,
        Weekday::Thu => CivilWeekday::Thursday,
        Weekday::Fri => CivilWeekday::Friday,
        Weekday::Sat => CivilWeekday::Saturday,
        Weekday::Sun => CivilWeekday::Sunday,
    }
}

const fn days_in_month(year: i32, month: i32) -> i32 {
    match month {
        2 if (year % 4 == 0 && year % 100 != 0) || year % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl CalendarRule {
    /// The rule's zone: [`Self::time_zone`] when set, else `local`.
    /// `None` for an unknown zone name.
    fn zone(&self, local: &TimeZone) -> Option<TimeZone> {
        self.time_zone
            .as_ref()
            .map_or_else(|| Some(local.clone()), |name| TimeZone::get(name).ok())
    }

    fn day_matches(&self, date: Date) -> bool {
        self.weekdays.is_empty()
            || self
                .weekdays
                .iter()
                .any(|day| civil(*day) == date.weekday())
    }

    /// The first occurrence strictly after `after_ms` (Unix-epoch
    /// milliseconds), in the device's local zone when the rule has none.
    /// `None` when there is no later one (past `until_ms`, an
    /// impossible or invalid rule).
    #[must_use]
    pub fn next_after(&self, after_ms: i64) -> Option<i64> {
        self.next_after_in(after_ms, &TimeZone::system())
    }

    /// [`Self::next_after`] with `local` standing in for the device's
    /// zone (tests, apps that check DST themselves).
    #[must_use]
    pub fn next_after_in(&self, after_ms: i64, local: &TimeZone) -> Option<i64> {
        if self.hour > 23 || self.minute > 59 {
            return None;
        }
        let tz = self.zone(local)?;
        let after = Timestamp::from_millisecond(after_ms).ok()?;
        // Start a day early: the previous day's time can still be ahead
        // of `after` around a DST transition at midnight.
        let start = tz.to_datetime(after).date().yesterday().ok()?;
        let (mut year, mut month) = (i32::from(start.year()), i32::from(start.month()));
        let mut first_day = i32::from(start.day());
        let last_year = year + SEARCH_YEARS;
        while year <= last_year {
            if self.month.is_none_or(|m| i32::from(m) == month) {
                let len = days_in_month(year, month);
                let days = self.day_of_month.map_or(Some(first_day..=len), |day| {
                    let day = i32::from(day);
                    (day >= first_day && day <= len).then_some(day..=day)
                });
                for day in days.into_iter().flatten() {
                    let date = Date::new(
                        i16::try_from(year).ok()?,
                        i8::try_from(month).ok()?,
                        i8::try_from(day).ok()?,
                    )
                    .ok()?;
                    if !self.day_matches(date) {
                        continue;
                    }
                    let wall = date.at(
                        i8::try_from(self.hour).ok()?,
                        i8::try_from(self.minute).ok()?,
                        0,
                        0,
                    );
                    let at = tz.to_ambiguous_timestamp(wall).compatible().ok()?;
                    let at_ms = at.as_millisecond();
                    if at_ms <= after_ms {
                        continue;
                    }
                    return match self.until_ms {
                        Some(until) if at_ms > until => None,
                        _ => Some(at_ms),
                    };
                }
            }
            first_day = 1;
            month += 1;
            if month > 12 {
                month = 1;
                year += 1;
            }
        }
        None
    }

    /// The next `count` occurrences after `from_ms`, in the device's
    /// local zone when the rule has none.
    #[must_use]
    pub fn next_occurrences(&self, from_ms: i64, count: usize) -> Vec<i64> {
        self.next_occurrences_in(from_ms, count, &TimeZone::system())
    }

    /// [`Self::next_occurrences`] with `local` standing in for the
    /// device's zone.
    #[must_use]
    pub fn next_occurrences_in(&self, from_ms: i64, count: usize, local: &TimeZone) -> Vec<i64> {
        let mut out = Vec::with_capacity(count.min(64));
        let mut after = from_ms;
        while out.len() < count {
            let Some(next) = self.next_after_in(after, local) else {
                break;
            };
            out.push(next);
            after = next;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(text: &str) -> i64 {
        text.parse::<Timestamp>().unwrap().as_millisecond()
    }

    #[test]
    fn daily_rule_in_a_fixed_zone() {
        let rule = CalendarRule::daily(8, 0).in_zone("Europe/Madrid");
        let next = rule.next_after_in(ms("2026-01-10T10:00:00Z"), &TimeZone::UTC);
        assert_eq!(next, Some(ms("2026-01-11T07:00:00Z")));
        let next = rule.next_after_in(ms("2026-01-10T06:59:59Z"), &TimeZone::UTC);
        assert_eq!(next, Some(ms("2026-01-10T07:00:00Z")));
        // Exactly at an occurrence: the next one.
        let next = rule.next_after_in(ms("2026-01-10T07:00:00Z"), &TimeZone::UTC);
        assert_eq!(next, Some(ms("2026-01-11T07:00:00Z")));
    }

    #[test]
    fn local_rules_follow_the_given_zone() {
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        let rule = CalendarRule::daily(9, 30);
        assert_eq!(
            rule.next_after_in(ms("2026-03-01T00:00:00Z"), &tokyo),
            Some(ms("2026-03-01T00:30:00Z"))
        );
    }

    #[test]
    fn until_stops_the_rule() {
        let rule = CalendarRule {
            until_ms: Some(ms("2026-01-12T07:00:00Z")),
            ..CalendarRule::daily(7, 0)
        };
        let got = rule.next_occurrences_in(ms("2026-01-10T00:00:00Z"), 10, &TimeZone::UTC);
        assert_eq!(
            got,
            [
                ms("2026-01-10T07:00:00Z"),
                ms("2026-01-11T07:00:00Z"),
                ms("2026-01-12T07:00:00Z")
            ]
        );
    }

    #[test]
    fn leap_day_rules_wait_for_leap_years() {
        let rule = CalendarRule::daily(12, 0).on_day(29).in_month(2);
        assert_eq!(
            rule.next_after_in(ms("2026-03-01T00:00:00Z"), &TimeZone::UTC),
            Some(ms("2028-02-29T12:00:00Z"))
        );
        // 2100 is not a leap year.
        assert_eq!(
            rule.next_after_in(ms("2097-01-01T00:00:00Z"), &TimeZone::UTC),
            Some(ms("2104-02-29T12:00:00Z"))
        );
    }

    #[test]
    fn impossible_rules_never_fire() {
        let rule = CalendarRule::daily(12, 0).on_day(31).in_month(4);
        assert_eq!(rule.next_after_in(0, &TimeZone::UTC), None);
        let bad = CalendarRule::daily(25, 0);
        assert_eq!(bad.next_after_in(0, &TimeZone::UTC), None);
        let zone = CalendarRule::daily(1, 0).in_zone("Nowhere/Never");
        assert_eq!(zone.next_after_in(0, &TimeZone::UTC), None);
    }
}
