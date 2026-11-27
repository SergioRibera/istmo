//! Which occurrences a scheduled toast may have shown: the backend's
//! record of what it (or an earlier process) handed to
//! `ToastNotifier.AddToSchedule`, by tag and fire time.
//!
//! The host delivers every occurrence itself too. Before such a show the
//! backend takes the occurrence it stands for ([`Marks::take_due`]) and
//! only then asks Windows whether that scheduled toast already popped;
//! ids without a mark skip the `WinRT` round trips.

use std::collections::HashMap;

/// A host delivery up to this far ahead of a fire time is that
/// occurrence's delivery.
pub(crate) const DELIVERY_SLACK_MS: i64 = 60_000;
/// How long a fired scheduled toast stands for its occurrence (a missed
/// delivery at the next start replaces it quietly within this window).
pub(crate) const SHOWN_WINDOW_MS: i64 = 24 * 3_600_000;
/// Fire times kept per tag (the arm cap, plus fired ones within the
/// window).
const MAX_PER_TAG: usize = 2 * crate::desktop::ARM_CAP;

/// Fire times of scheduled toasts, by tag, ascending.
#[derive(Debug, Default)]
pub(crate) struct Marks {
    by_tag: HashMap<String, Vec<i64>>,
}

impl Marks {
    /// A scheduled toast of `tag` fires (or fired) at `fire_ms`.
    pub(crate) fn mark(&mut self, tag: &str, fire_ms: i64) {
        let times = self.by_tag.entry(tag.to_owned()).or_default();
        if let Err(at) = times.binary_search(&fire_ms) {
            times.insert(at, fire_ms);
            if times.len() > MAX_PER_TAG {
                times.remove(0);
            }
        }
    }

    /// The scheduled toast of `tag` at `fire_ms` left the schedule before
    /// it fired.
    pub(crate) fn unmark(&mut self, tag: &str, fire_ms: i64) {
        if let Some(times) = self.by_tag.get_mut(tag)
            && let Ok(at) = times.binary_search(&fire_ms)
        {
            times.remove(at);
            if times.is_empty() {
                self.by_tag.remove(tag);
            }
        }
    }

    /// The occurrence a host delivery of `tag` at `now` stands for: the
    /// latest fire time within [`SHOWN_WINDOW_MS`] before and
    /// [`DELIVERY_SLACK_MS`] after `now`. It and every earlier one of the
    /// tag are forgotten (delivered by the host, or skipped).
    pub(crate) fn take_due(&mut self, tag: &str, now: i64) -> Option<i64> {
        let times = self.by_tag.get_mut(tag)?;
        let ahead = now.saturating_add(DELIVERY_SLACK_MS);
        let end = times.partition_point(|&at| at <= ahead);
        let fire = times[..end].last().copied();
        times.drain(..end);
        if times.is_empty() {
            self.by_tag.remove(tag);
        }
        fire.filter(|&at| at >= now.saturating_sub(SHOWN_WINDOW_MS))
    }

    /// Forgets fire times older than [`SHOWN_WINDOW_MS`].
    pub(crate) fn prune(&mut self, now: i64) {
        let oldest = now.saturating_sub(SHOWN_WINDOW_MS);
        self.by_tag.retain(|_, times| {
            times.retain(|&at| at >= oldest);
            !times.is_empty()
        });
    }

    /// Whether any fire time is kept.
    #[cfg(test)]
    pub(crate) fn is_empty(&self) -> bool {
        self.by_tag.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOUR: i64 = 3_600_000;

    #[test]
    fn unmarked_tags_have_nothing_due() {
        let mut marks = Marks::default();
        assert_eq!(marks.take_due("a", 0), None);
        marks.mark("a", 10 * HOUR);
        assert_eq!(marks.take_due("b", 10 * HOUR), None);
        // Not due yet: kept.
        assert_eq!(marks.take_due("a", 9 * HOUR), None);
        assert_eq!(marks.take_due("a", 10 * HOUR), Some(10 * HOUR));
        assert!(marks.is_empty());
    }

    #[test]
    fn the_latest_due_occurrence_wins_and_older_ones_go() {
        let mut marks = Marks::default();
        for at in [HOUR, 2 * HOUR, 3 * HOUR, 4 * HOUR] {
            marks.mark("r", at);
        }
        // A late delivery at 3h10 stands for 3h; 4h stays armed.
        assert_eq!(marks.take_due("r", 3 * HOUR + 600_000), Some(3 * HOUR));
        assert_eq!(marks.take_due("r", 3 * HOUR + 700_000), None);
        assert_eq!(marks.take_due("r", 4 * HOUR - 30_000), Some(4 * HOUR));
    }

    #[test]
    fn old_and_unscheduled_occurrences_are_forgotten() {
        let mut marks = Marks::default();
        marks.mark("a", HOUR);
        marks.mark("a", 2 * HOUR);
        marks.unmark("a", 2 * HOUR);
        // Older than the window: not this delivery's toast.
        assert_eq!(marks.take_due("a", HOUR + SHOWN_WINDOW_MS + 1), None);
        marks.mark("b", HOUR);
        marks.prune(HOUR + SHOWN_WINDOW_MS + 1);
        assert!(marks.is_empty());
    }

    #[test]
    fn marks_per_tag_are_bounded() {
        let mut marks = Marks::default();
        for i in 0..1_000 {
            marks.mark("a", i);
        }
        assert_eq!(marks.by_tag["a"].len(), MAX_PER_TAG);
        assert_eq!(
            marks.by_tag["a"][0],
            1_000 - i64::try_from(MAX_PER_TAG).unwrap()
        );
    }
}
