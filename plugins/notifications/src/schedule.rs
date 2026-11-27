//! Scheduling: calendar rule math, trigger timing, the in-process
//! engine and its store.
//!
//! * [`CalendarRule::next_after`](crate::CalendarRule::next_after): rule
//!   math (`rule.rs`).
//! * [`Trigger::next_after`](crate::Trigger::next_after): when a trigger
//!   armed at some instant fires next.
//! * [`Engine`]: the scheduled entries of a host that schedules
//!   in-process (Linux, the web, the desktop fallback). Pure: time comes
//!   in as arguments, nothing runs on its own; the host arms one
//!   [`Wakeup`] for [`Engine::next_deadline`].
//! * [`Store`]: the engine's entries on disk (format byte + wincode,
//!   atomic replace).
//!
//! Native schedulers (Android `AlarmManager`, Apple triggers, Windows
//! scheduled toasts) keep their own entries but use the same timing
//! rules, so `pending()` reports the same `next_fire_ms` everywhere.

mod engine;
mod rule;
mod store;

pub use engine::{Due, Engine, Entry, LATE_AFTER_MS};
pub use store::{STORE_FORMAT, Store, StoreError};

use jiff::tz::TimeZone;

use crate::Trigger;

/// Something that calls the host back at a deadline: one shared timer
/// thread on desktop, a `setTimeout` on the web.
///
/// Implementations keep one pending wake-up; every call replaces the
/// previous one. `None` cancels it.
pub trait Wakeup: Send + Sync {
    /// Wake the host at `deadline_ms` (Unix-epoch milliseconds; at once
    /// when past).
    fn arm(&self, deadline_ms: Option<i64>);
}

impl Trigger {
    /// The first occurrence strictly after `after_ms` of this trigger,
    /// armed at `anchor_ms`, in the device's local zone. Occurrences:
    ///
    /// * `After(d)`: `anchor + d`, once.
    /// * `At(t)`: `t`, once.
    /// * `Every(p)`: `anchor + k * p` for `k >= 1`.
    /// * `Calendar(rule)`: the rule's occurrences after `anchor`.
    #[must_use]
    pub fn next_after(&self, anchor_ms: i64, after_ms: i64) -> Option<i64> {
        self.next_after_in(anchor_ms, after_ms, &TimeZone::system())
    }

    /// [`Self::next_after`] with `local` standing in for the device's zone.
    #[must_use]
    pub fn next_after_in(&self, anchor_ms: i64, after_ms: i64, local: &TimeZone) -> Option<i64> {
        match self {
            Self::After(delay) => {
                let at = anchor_ms.saturating_add(i64::try_from(*delay).unwrap_or(i64::MAX));
                (at > after_ms).then_some(at)
            }
            Self::At(at) => (*at > after_ms).then_some(*at),
            Self::Every(period) => {
                let period = i64::try_from(*period).unwrap_or(i64::MAX).max(1);
                let elapsed = after_ms.saturating_sub(anchor_ms);
                let k = if elapsed < 0 { 1 } else { elapsed / period + 1 };
                Some(anchor_ms.saturating_add(k.saturating_mul(period)))
            }
            Self::Calendar(rule) => rule.next_after_in(after_ms.max(anchor_ms), local),
        }
    }

    /// The first delivery of this trigger armed at `anchor_ms`, in the
    /// device's local zone. A time already past (`At` in the past)
    /// delivers at once.
    #[must_use]
    pub fn first_fire(&self, anchor_ms: i64) -> Option<i64> {
        self.first_fire_in(anchor_ms, &TimeZone::system())
    }

    /// [`Self::first_fire`] with `local` standing in for the device's zone.
    #[must_use]
    pub fn first_fire_in(&self, anchor_ms: i64, local: &TimeZone) -> Option<i64> {
        match self {
            Self::At(at) => Some((*at).max(anchor_ms)),
            _ => self.next_after_in(anchor_ms, anchor_ms.saturating_sub(1), local),
        }
    }

    /// Whether it fires more than once.
    #[must_use]
    pub const fn repeats(&self) -> bool {
        matches!(self, Self::Every(_) | Self::Calendar(_))
    }
}

#[cfg(test)]
mod tests {
    use crate::{CalendarRule, Trigger};

    #[test]
    fn one_shot_triggers_fire_once() {
        assert_eq!(Trigger::After(500).first_fire(1_000), Some(1_500));
        assert_eq!(Trigger::After(500).next_after(1_000, 1_500), None);
        assert_eq!(Trigger::At(5_000).first_fire(1_000), Some(5_000));
        // Already past: at once.
        assert_eq!(Trigger::At(500).first_fire(1_000), Some(1_000));
        assert!(!Trigger::At(0).repeats());
    }

    #[test]
    fn intervals_stay_on_their_grid() {
        let every = Trigger::Every(60_000);
        assert_eq!(every.first_fire(0), Some(60_000));
        assert_eq!(every.next_after(0, 60_000), Some(120_000));
        // Missed several: the next one on the grid, not one period from now.
        assert_eq!(every.next_after(0, 250_000), Some(300_000));
        assert!(every.repeats());
    }

    #[test]
    fn calendar_triggers_start_after_their_anchor() {
        let rule = Trigger::Calendar(CalendarRule::daily(0, 0).in_zone("UTC"));
        assert_eq!(rule.first_fire(0), Some(86_400_000));
        assert_eq!(rule.next_after(0, 86_400_000), Some(2 * 86_400_000));
    }
}
