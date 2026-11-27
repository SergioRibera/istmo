//! The in-process scheduler: entries, deadlines, missed deliveries.

use std::collections::HashMap;
use std::sync::Arc;

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
    /// Next delivery; `None` once it will never fire again (the engine
    /// never keeps such an entry).
    pub next_fire_ms: Option<i64>,
}

impl Entry {
    /// Soonest first, ties by id.
    pub(crate) fn order(&self) -> (i64, &NotificationId) {
        (self.next_fire_ms.unwrap_or(i64::MAX), &self.notification.id)
    }

    /// A calendar rule without its own zone: follows the device's zone.
    const fn floating(&self) -> bool {
        matches!(&self.trigger, Trigger::Calendar(rule) if rule.time_zone.is_none())
    }
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
///
/// Entries are shared (`Arc`) so a host can take a [`snapshot`] under
/// its lock and encode it outside.
///
/// [`snapshot`]: Self::snapshot
#[derive(Debug)]
pub struct Engine {
    entries: HashMap<NotificationId, Arc<Entry>>,
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
    /// Entries that never fire again are dropped.
    #[must_use]
    pub fn with_entries(local: TimeZone, entries: Vec<Entry>) -> Self {
        let entries = entries
            .into_iter()
            .filter(|entry| entry.next_fire_ms.is_some())
            .map(|entry| (entry.notification.id.clone(), Arc::new(entry)))
            .collect();
        Self { entries, local }
    }

    /// Schedules `notification` (replacing one with the same id) at
    /// `now_ms`. A trigger that never fires (a calendar rule past its
    /// `until_ms`, or with no matching day) is not kept and replaces
    /// nothing: the result's `next_fire_ms` is `None`.
    pub fn add(
        &mut self,
        notification: Notification,
        trigger: Trigger,
        now_ms: i64,
    ) -> ScheduledInfo {
        let next_fire_ms = trigger.first_fire_in(now_ms, &self.local);
        if next_fire_ms.is_some() {
            self.entries.insert(
                notification.id.clone(),
                Arc::new(Entry {
                    notification,
                    trigger,
                    anchor_ms: now_ms,
                    next_fire_ms,
                }),
            );
        }
        ScheduledInfo {
            exact: true,
            next_fire_ms,
        }
    }

    /// Moves every entry of `other` in, replacing entries with the same
    /// id.
    pub fn merge(&mut self, other: Self) {
        self.entries.extend(other.entries);
    }

    /// The zone of rules without one.
    #[must_use]
    pub const fn local(&self) -> &TimeZone {
        &self.local
    }

    /// The device's zone changed to `local` (checked at `now_ms`): rules
    /// without a zone of their own move to their next occurrence in it
    /// (ones already due stay due). Whether anything changed.
    pub fn set_local(&mut self, local: TimeZone, now_ms: i64) -> bool {
        if local == self.local {
            return false;
        }
        self.local = local;
        let local = &self.local;
        self.entries.retain(|_, entry| {
            if !entry.floating() || entry.next_fire_ms.is_some_and(|at| at <= now_ms) {
                return true;
            }
            let next = entry.trigger.next_after_in(entry.anchor_ms, now_ms, local);
            Arc::make_mut(entry).next_fire_ms = next;
            next.is_some()
        });
        true
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

    /// Every entry, soonest first, ties by id.
    #[must_use]
    pub fn entries(&self) -> Vec<&Entry> {
        let mut all: Vec<&Entry> = self.entries.values().map(AsRef::as_ref).collect();
        all.sort_by(|a, b| a.order().cmp(&b.order()));
        all
    }

    /// Every entry, shared and unsorted: cheap to take under a lock
    /// (reference counts only); sort and encode it outside.
    #[must_use]
    pub fn snapshot(&self) -> Vec<Arc<Entry>> {
        self.entries.values().cloned().collect()
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
    /// (several missed occurrences deliver at most once); a repeat with
    /// no next occurrence leaves the engine too.
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
                Arc::make_mut(entry).next_fire_ms = next;
                deliver.map(|_| entry.notification.clone())
            } else {
                self.entries
                    .remove(&id)
                    .and_then(|entry| deliver.map(|_| Arc::unwrap_or_clone(entry).notification))
            };
            if let (Some(notification), Some(late)) = (notification, deliver) {
                out.push(Due { notification, late });
            }
        }
        out
    }
}

/// Calendar occurrences walked forward from the start found by
/// [`latest_occurrence`]'s search; a rule denser than this inside the
/// searched window stops early at an earlier occurrence (lateness is then
/// over-estimated, never under).
const MAX_MISSED_WALK: usize = 4_096;

/// First probe of the backward search, milliseconds.
const FIRST_PROBE_MS: i64 = 1_000;

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
            let next = |after: i64| entry.trigger.next_after_in(entry.anchor_ms, after, local);
            // Search back from now, doubling the window, for the first
            // window holding an occurrence: the walk then starts close
            // to the latest one however long ago `at` was.
            let mut last = at;
            let mut back = FIRST_PROBE_MS;
            loop {
                let probe = now_ms.saturating_sub(back);
                if probe <= at {
                    break;
                }
                if let Some(found) = next(probe).filter(|&found| found <= now_ms) {
                    last = found;
                    break;
                }
                back = back.saturating_mul(2);
            }
            for _ in 0..MAX_MISSED_WALK {
                match next(last) {
                    Some(found) if found <= now_ms => last = found,
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

    #[test]
    fn dead_entries_are_never_kept() {
        let mut e = engine();
        let ended = CalendarRule {
            until_ms: Some(1_000),
            ..CalendarRule::daily(9, 0).in_zone("UTC")
        };
        e.add(n("x"), Trigger::After(10), 0);
        // A rule past its end replaces nothing and is not kept.
        let info = e.add(n("x"), Trigger::Calendar(ended.clone()), 86_400_000);
        assert_eq!(info.next_fire_ms, None);
        assert_eq!(e.pending()[0].trigger, Trigger::After(10));
        // Read from a store: dropped.
        let stored = Entry {
            notification: n("dead"),
            trigger: Trigger::Calendar(ended),
            anchor_ms: 0,
            next_fire_ms: None,
        };
        assert!(Engine::with_entries(TimeZone::UTC, vec![stored]).is_empty());
    }

    #[test]
    fn lateness_counts_from_the_latest_occurrence_however_long_ago() {
        let mut e = engine();
        let rule = CalendarRule::daily(9, 0).in_zone("UTC");
        e.add(n("daily"), Trigger::Calendar(rule), 0);
        // Twenty years asleep (more days than the forward walk takes),
        // woken half an hour after a 9:00: half an hour late, delivered.
        let day = 20 * 366;
        let now = day * 86_400_000 + 9 * 3_600_000 + 1_800_000;
        let fired = e.due(now);
        assert_eq!(fired.len(), 1);
        assert!(fired[0].late);
    }

    #[test]
    fn floating_rules_follow_a_new_local_zone() {
        let mut e = Engine::new(TimeZone::UTC);
        e.add(n("local"), Trigger::Calendar(CalendarRule::daily(9, 0)), 0);
        e.add(
            n("fixed"),
            Trigger::Calendar(CalendarRule::daily(9, 0).in_zone("UTC")),
            0,
        );
        assert_eq!(e.next_deadline(), Some(9 * 3_600_000));
        assert!(!e.set_local(TimeZone::UTC, 0));
        let tokyo = TimeZone::get("Asia/Tokyo").unwrap();
        assert!(e.set_local(tokyo, 0));
        let next: Vec<_> = e
            .entries()
            .into_iter()
            .map(|entry| (entry.notification.id.value.clone(), entry.next_fire_ms))
            .collect();
        // 09:00 in Tokyo is 00:00 UTC.
        assert_eq!(
            next,
            [
                ("fixed".to_owned(), Some(9 * 3_600_000)),
                ("local".to_owned(), Some(86_400_000)),
            ]
        );
    }
}
