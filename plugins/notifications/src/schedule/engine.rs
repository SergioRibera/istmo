//! The in-process scheduler: entries, deadlines, missed deliveries.

use std::collections::HashMap;

use jiff::tz::TimeZone;

use crate::{
    GroupId, MissedPolicy, Notification, NotificationId, PendingNotification, ScheduledInfo,
    Trigger,
};

/// A delivery this much later than its time counts as missed (the
/// process was not running, the machine slept) and goes through its
/// [`MissedPolicy`].
pub const LATE_AFTER_MS: i64 = 60_000;

/// One scheduled notification.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// What to show.
    pub notification: Notification,
    /// When.
    pub trigger: Trigger,
    /// When it was scheduled, Unix-epoch milliseconds.
    pub anchor_ms: i64,
    /// Next delivery; `None` once it will never fire again.
    pub next_fire_ms: Option<i64>,
}

/// A notification whose time came.
#[derive(Debug, Clone, PartialEq)]
pub struct Due {
    /// What to show.
    pub notification: Notification,
    /// Delivered late, within its [`MissedPolicy::DeliverLate`] window.
    pub late: bool,
}

/// The scheduled entries of one host.
#[derive(Debug)]
pub struct Engine {
    entries: HashMap<NotificationId, Entry>,
    local: TimeZone,
}

impl Default for Engine {
    fn default() -> Self {
        Self::new(TimeZone::system())
    }
}

impl Engine {
    /// An empty engine; `local` is the zone of rules without one.
    #[must_use]
    pub fn new(local: TimeZone) -> Self {
        Self {
            entries: HashMap::new(),
            local,
        }
    }

    /// An engine holding `entries` (read from a [`super::Store`]).
    #[must_use]
    pub fn with_entries(local: TimeZone, entries: Vec<Entry>) -> Self {
        let entries = entries
            .into_iter()
            .map(|entry| (entry.notification.id.clone(), entry))
            .collect();
        Self { entries, local }
    }

    /// Schedules `notification` (replacing one with the same id) at
    /// `now_ms`.
    pub fn add(
        &mut self,
        notification: Notification,
        trigger: Trigger,
        now_ms: i64,
    ) -> ScheduledInfo {
        let next_fire_ms = trigger.first_fire_in(now_ms, &self.local);
        self.entries.insert(
            notification.id.clone(),
            Entry {
                notification,
                trigger,
                anchor_ms: now_ms,
                next_fire_ms,
            },
        );
        ScheduledInfo {
            exact: true,
            next_fire_ms,
        }
    }

    /// Removes `id`; whether it was scheduled.
    pub fn remove(&mut self, id: &NotificationId) -> bool {
        self.entries.remove(id).is_some()
    }

    /// Removes every entry of `group`; the removed ids.
    pub fn remove_group(&mut self, group: &GroupId) -> Vec<NotificationId> {
        let ids: Vec<_> = self
            .entries
            .values()
            .filter(|entry| {
                entry
                    .notification
                    .group
                    .as_ref()
                    .is_some_and(|g| &g.id == group)
            })
            .map(|entry| entry.notification.id.clone())
            .collect();
        for id in &ids {
            self.entries.remove(id);
        }
        ids
    }

    /// Removes everything.
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Whether `id` is scheduled.
    #[must_use]
    pub fn contains(&self, id: &NotificationId) -> bool {
        self.entries.contains_key(id)
    }

    /// Number of entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether nothing is scheduled.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every entry, soonest first (never-firing ones last), ties by id.
    #[must_use]
    pub fn entries(&self) -> Vec<&Entry> {
        let mut all: Vec<_> = self.entries.values().collect();
        all.sort_by(|a, b| {
            let key = |e: &Entry| {
                (
                    e.next_fire_ms.unwrap_or(i64::MAX),
                    e.notification.id.clone(),
                )
            };
            key(a).cmp(&key(b))
        });
        all
    }

    /// [`Self::entries`] as the wire type.
    #[must_use]
    pub fn pending(&self) -> Vec<PendingNotification> {
        self.entries()
            .into_iter()
            .map(|entry| PendingNotification {
                notification: entry.notification.clone(),
                trigger: entry.trigger.clone(),
                next_fire_ms: entry.next_fire_ms,
            })
            .collect()
    }

    /// The soonest delivery.
    #[must_use]
    pub fn next_deadline(&self) -> Option<i64> {
        self.entries
            .values()
            .filter_map(|entry| entry.next_fire_ms)
            .min()
    }

    /// Takes every entry due at `now_ms`: one-shots leave the engine,
    /// repeating ones move to their next occurrence after `now_ms`
    /// (several missed occurrences deliver at most once).
    ///
    /// An entry more than [`LATE_AFTER_MS`] late follows its
    /// [`Notification::missed`] policy: [`MissedPolicy::Drop`] skips it,
    /// [`MissedPolicy::DeliverLate`] delivers it (`late = true`) when it
    /// is at most that late. Results are in deadline order.
    pub fn due(&mut self, now_ms: i64) -> Vec<Due> {
        let mut ready: Vec<(i64, NotificationId)> = self
            .entries
            .values()
            .filter_map(|entry| {
                let at = entry.next_fire_ms?;
                (at <= now_ms).then(|| (at, entry.notification.id.clone()))
            })
            .collect();
        ready.sort();
        let mut out = Vec::with_capacity(ready.len());
        for (at, id) in ready {
            let Some(entry) = self.entries.get_mut(&id) else {
                continue;
            };
            let lateness = now_ms - latest_occurrence(entry, at, now_ms, &self.local);
            let deliver = if lateness <= LATE_AFTER_MS {
                Some(false)
            } else {
                match entry.notification.missed {
                    MissedPolicy::Drop => None,
                    MissedPolicy::DeliverLate(max) => {
                        let max = i64::try_from(max).unwrap_or(i64::MAX);
                        (lateness <= max).then_some(true)
                    }
                }
            };
            let next = entry
                .trigger
                .next_after_in(entry.anchor_ms, now_ms, &self.local);
            let notification = if next.is_some() {
                entry.next_fire_ms = next;
                deliver.map(|_| entry.notification.clone())
            } else {
                self.entries
                    .remove(&id)
                    .and_then(|entry| deliver.map(|_| entry.notification))
            };
            if let (Some(notification), Some(late)) = (notification, deliver) {
                out.push(Due { notification, late });
            }
        }
        out
    }
}

/// Calendar occurrences walked to find the latest missed one; past this
/// the oldest missed one counts (it is late either way).
const MAX_MISSED_WALK: usize = 4_096;

/// The latest occurrence of `entry` at or before `now_ms`, starting from
/// the due one at `at`: lateness counts from the occurrence missed last,
/// so a daily reminder missed for three days is as late as today's.
fn latest_occurrence(entry: &Entry, at: i64, now_ms: i64, local: &TimeZone) -> i64 {
    match &entry.trigger {
        Trigger::After(_) | Trigger::At(_) => at,
        Trigger::Every(period) => {
            let period = i64::try_from(*period).unwrap_or(i64::MAX).max(1);
            let steps = (now_ms - entry.anchor_ms) / period;
            entry
                .anchor_ms
                .saturating_add(steps.saturating_mul(period))
                .max(at)
        }
        Trigger::Calendar(_) => {
            let mut last = at;
            for _ in 0..MAX_MISSED_WALK {
                match entry.trigger.next_after_in(entry.anchor_ms, last, local) {
                    Some(next) if next <= now_ms => last = next,
                    _ => break,
                }
            }
            last
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CalendarRule;

    fn n(id: &str) -> Notification {
        Notification::new(id, "t").unwrap()
    }

    fn engine() -> Engine {
        Engine::new(TimeZone::UTC)
    }

    #[test]
    fn one_shots_fire_once_in_deadline_order() {
        let mut e = engine();
        e.add(n("b"), Trigger::After(2_000), 0);
        let info = e.add(n("a"), Trigger::After(1_000), 0);
        assert_eq!(info.next_fire_ms, Some(1_000));
        assert_eq!(e.next_deadline(), Some(1_000));
        assert_eq!(e.due(999), Vec::new());
        let fired: Vec<_> = e
            .due(2_000)
            .into_iter()
            .map(|d| (d.notification.id.value, d.late))
            .collect();
        assert_eq!(fired, [("a".to_owned(), false), ("b".to_owned(), false)]);
        assert!(e.is_empty());
        assert_eq!(e.next_deadline(), None);
    }

    #[test]
    fn repeats_move_to_their_next_occurrence() {
        let mut e = engine();
        e.add(n("r"), Trigger::Every(60_000), 0);
        assert_eq!(e.due(60_000).len(), 1);
        assert_eq!(e.next_deadline(), Some(120_000));
        assert_eq!(e.pending()[0].next_fire_ms, Some(120_000));
    }

    #[test]
    fn missed_deliveries_follow_their_policy() {
        let mut e = engine();
        e.add(n("late"), Trigger::At(1_000), 0);
        e.add(
            n("drop").with_missed(MissedPolicy::Drop),
            Trigger::At(1_000),
            0,
        );
        e.add(
            n("stale").with_missed(MissedPolicy::DeliverLate(10_000)),
            Trigger::At(1_000),
            0,
        );
        // Two minutes late: past the grace, inside the default hour.
        let fired: Vec<_> = e
            .due(121_000)
            .into_iter()
            .map(|d| (d.notification.id.value, d.late))
            .collect();
        assert_eq!(fired, [("late".to_owned(), true)]);
        assert!(e.is_empty());
    }

    #[test]
    fn missed_repeats_deliver_once_and_rejoin_the_rule() {
        let mut e = engine();
        let rule = CalendarRule::daily(9, 0).in_zone("UTC");
        e.add(n("daily"), Trigger::Calendar(rule), 0);
        // Asleep for three days, woken half an hour after the 9:00 of day 3.
        let now = 3 * 86_400_000 + 9 * 3_600_000 + 1_800_000;
        let fired = e.due(now);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].late);
        assert_eq!(e.next_deadline(), Some(4 * 86_400_000 + 9 * 3_600_000));
    }

    #[test]
    fn groups_and_ids_are_removed() {
        let mut e = engine();
        e.add(n("a").with_group("g").unwrap(), Trigger::After(1), 0);
        e.add(n("b").with_group("g").unwrap(), Trigger::After(1), 0);
        e.add(n("c"), Trigger::After(1), 0);
        let mut removed = e.remove_group(&GroupId::try_from("g").unwrap());
        removed.sort();
        assert_eq!(removed.len(), 2);
        assert!(e.remove(&NotificationId::try_from("c").unwrap()));
        assert!(e.is_empty());
    }
}
