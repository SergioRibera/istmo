//! Live-update coalescing: the latest pending update per notification,
//! posted no faster than a per-app token bucket allows.
//!
//! Platforms drop or throttle updates above a few per second (Android
//! about 5 enqueues per second per package). The [`Coalescer`] keeps the
//! posting rate under that:
//!
//! * Per id the latest pending update wins; older ones are never posted.
//! * A token bucket (rate per second, burst) limits posts per app.
//! * [`Urgency::Alert`] (`alert_again`) and [`Urgency::Finish`] jump the
//!   queue: posted at once, they still spend a token, so the bucket may
//!   go into debt and regular updates wait longer. The debt is floored at
//!   `burst` tokens: a storm of alerts delays regular updates by at most
//!   `2 * burst / per_second` seconds, never without bound.
//! * [`Coalescer::cancel`] drops what is queued for an id.
//! * An update for a notification the user dismissed is dropped, unless
//!   it alerts (which shows it again). A finish is a new show and always
//!   passes.
//!
//! The coalescer is deterministic: time comes in as milliseconds from
//! the caller's clock, nothing runs on its own. Callers post what
//! [`Coalescer::offer`] returns at once, and call [`Coalescer::poll`]
//! when [`Coalescer::next_due`] says the next queued update may go.
//!
//! Kotlin (`native/android/Coalescer.kt`) and Swift
//! (`native/ios/Coalescer.swift`) ports must keep these rules; the unit
//! tests below are the reference cases.

use std::collections::{HashMap, HashSet, VecDeque};

use crate::NotificationId;

/// Milli-tokens per token: integer arithmetic keeps the bucket exact.
const TOKEN: i64 = 1_000;

/// Posting rate of one backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    /// Posts per second, sustained.
    pub per_second: u32,
    /// Posts allowed in a burst.
    pub burst: u32,
}

impl Rate {
    /// Android: 4 per second, burst 4 (the system drops above about 5).
    pub const ANDROID: Self = Self::new(4, 4);
    /// Apple: 2 per second, burst 2.
    pub const APPLE: Self = Self::new(2, 2);
    /// Windows toast data binding: 20 per second.
    pub const WINDOWS: Self = Self::new(20, 20);
    /// Linux notification servers: 10 per second.
    pub const LINUX: Self = Self::new(10, 10);
    /// Web: 4 per second.
    pub const WEB: Self = Self::new(4, 4);

    /// `per_second` posts per second with bursts of `burst` (both at
    /// least 1).
    #[must_use]
    pub const fn new(per_second: u32, burst: u32) -> Self {
        Self {
            per_second: if per_second == 0 { 1 } else { per_second },
            burst: if burst == 0 { 1 } else { burst },
        }
    }
}

/// How an update is treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Urgency {
    /// A silent update: coalesced, rate limited, dropped once dismissed.
    Regular,
    /// Alerts again: posted at once, shows a dismissed one again.
    Alert,
    /// The final content: posted at once, always.
    Finish,
}

/// What to do with an offered update.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Admit<T> {
    /// Post it now.
    Now(T),
    /// Held; it (or a newer one) comes out of [`Coalescer::poll`].
    Queued,
    /// Dropped: the user dismissed the notification.
    Dropped,
}

/// The per-app coalescer. `T` is whatever the caller posts.
#[derive(Debug)]
pub struct Coalescer<T> {
    rate: Rate,
    /// Milli-tokens; negative after bypasses, never below `-burst`
    /// tokens.
    tokens: i64,
    /// Clock of the last refill.
    refilled_ms: i64,
    /// Ids with a queued update, oldest first.
    order: VecDeque<NotificationId>,
    pending: HashMap<NotificationId, T>,
    dismissed: HashSet<NotificationId>,
}

impl<T> Coalescer<T> {
    /// A coalescer with a full bucket at `now_ms`.
    #[must_use]
    pub fn new(rate: Rate, now_ms: i64) -> Self {
        Self {
            rate,
            tokens: i64::from(rate.burst) * TOKEN,
            refilled_ms: now_ms,
            order: VecDeque::new(),
            pending: HashMap::new(),
            dismissed: HashSet::new(),
        }
    }

    /// The rate in use.
    #[must_use]
    pub const fn rate(&self) -> Rate {
        self.rate
    }

    fn refill(&mut self, now_ms: i64) {
        let elapsed = now_ms.saturating_sub(self.refilled_ms).max(0);
        self.refilled_ms = self.refilled_ms.max(now_ms);
        let full = i64::from(self.rate.burst) * TOKEN;
        // Milli-tokens per millisecond = tokens per second.
        let gained = elapsed.saturating_mul(i64::from(self.rate.per_second));
        self.tokens = self.tokens.saturating_add(gained).min(full);
    }

    /// Spends a token even when none is left (alerts, finishes, full
    /// shows), down to the debt floor.
    fn spend(&mut self) {
        let floor = -i64::from(self.rate.burst) * TOKEN;
        self.tokens = (self.tokens - TOKEN).max(floor);
    }

    /// Switches to `rate` at `now_ms`, keeping what is queued: the
    /// bucket refills at the old rate up to `now_ms`, then is clamped to
    /// the new burst (debt included).
    pub fn set_rate(&mut self, rate: Rate, now_ms: i64) {
        if rate == self.rate {
            return;
        }
        self.refill(now_ms);
        self.rate = rate;
        let full = i64::from(rate.burst) * TOKEN;
        self.tokens = self.tokens.clamp(-full, full);
    }

    const fn take(&mut self) -> bool {
        if self.tokens >= TOKEN {
            self.tokens -= TOKEN;
            true
        } else {
            false
        }
    }

    fn unqueue(&mut self, id: &NotificationId) -> Option<T> {
        let item = self.pending.remove(id)?;
        if let Some(at) = self.order.iter().position(|queued| queued == id) {
            self.order.remove(at);
        }
        Some(item)
    }

    /// An update for `id` at `now_ms`.
    pub fn offer(
        &mut self,
        id: &NotificationId,
        item: T,
        urgency: Urgency,
        now_ms: i64,
    ) -> Admit<T> {
        self.refill(now_ms);
        match urgency {
            Urgency::Regular => {
                if self.dismissed.contains(id) {
                    return Admit::Dropped;
                }
                if let Some(slot) = self.pending.get_mut(id) {
                    *slot = item;
                    return Admit::Queued;
                }
                // Queued updates of other ids go first.
                if self.order.is_empty() && self.take() {
                    return Admit::Now(item);
                }
                self.order.push_back(id.clone());
                self.pending.insert(id.clone(), item);
                Admit::Queued
            }
            Urgency::Alert | Urgency::Finish => {
                self.dismissed.remove(id);
                self.unqueue(id);
                self.spend();
                Admit::Now(item)
            }
        }
    }

    /// A full show of `id` (not an update): forgets its dismissal and
    /// its queued update, spends a token.
    pub fn shown(&mut self, id: &NotificationId, now_ms: i64) {
        self.refill(now_ms);
        self.dismissed.remove(id);
        self.unqueue(id);
        self.spend();
    }

    /// The user dismissed `id`: its queued update is dropped and later
    /// regular ones are too.
    pub fn dismissed(&mut self, id: &NotificationId) {
        self.unqueue(id);
        self.dismissed.insert(id.clone());
    }

    /// `id` is gone for good (cancelled by the app): drops its queued
    /// update and its dismissal.
    pub fn cancel(&mut self, id: &NotificationId) {
        self.unqueue(id);
        self.dismissed.remove(id);
    }

    /// Forgets everything queued and dismissed.
    pub fn clear(&mut self) {
        self.order.clear();
        self.pending.clear();
        self.dismissed.clear();
    }

    /// The queued updates that may go at `now_ms`, oldest first.
    pub fn poll(&mut self, now_ms: i64) -> Vec<(NotificationId, T)> {
        self.refill(now_ms);
        let mut out = Vec::new();
        while !self.order.is_empty() && self.take() {
            if let Some(id) = self.order.pop_front()
                && let Some(item) = self.pending.remove(&id)
            {
                out.push((id, item));
            }
        }
        out
    }

    /// When the next queued update may go, `None` when nothing is queued.
    #[must_use]
    pub fn next_due(&self) -> Option<i64> {
        if self.order.is_empty() {
            return None;
        }
        let missing = (TOKEN - self.tokens).max(0);
        let per_ms = i64::from(self.rate.per_second);
        // Round up: a token is whole only after the full wait.
        let wait = (missing + per_ms - 1) / per_ms;
        Some(self.refilled_ms.saturating_add(wait))
    }

    /// Number of queued updates.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.order.len()
    }

    /// Number of ids remembered as dismissed.
    #[must_use]
    pub fn dismissed_len(&self) -> usize {
        self.dismissed.len()
    }

    /// Whether `id` was dismissed by the user.
    #[must_use]
    pub fn is_dismissed(&self, id: &NotificationId) -> bool {
        self.dismissed.contains(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(value: &str) -> NotificationId {
        NotificationId::try_from(value).unwrap()
    }

    #[test]
    fn a_burst_goes_then_updates_coalesce_per_id() {
        let mut c = Coalescer::new(Rate::new(2, 2), 0);
        let a = id("a");
        assert_eq!(c.offer(&a, 1, Urgency::Regular, 0), Admit::Now(1));
        assert_eq!(c.offer(&a, 2, Urgency::Regular, 0), Admit::Now(2));
        // The bucket is empty: the rest is held, the latest wins.
        assert_eq!(c.offer(&a, 3, Urgency::Regular, 0), Admit::Queued);
        assert_eq!(c.offer(&a, 4, Urgency::Regular, 10), Admit::Queued);
        assert_eq!(c.queued(), 1);
        // 2 per second: a token every 500 ms.
        assert_eq!(c.next_due(), Some(500));
        assert_eq!(c.poll(499), Vec::new());
        assert_eq!(c.poll(500), vec![(a, 4)]);
        assert_eq!(c.next_due(), None);
    }

    #[test]
    fn queued_ids_go_in_arrival_order_and_new_ones_wait_behind_them() {
        let mut c = Coalescer::new(Rate::new(1, 1), 0);
        let (a, b) = (id("a"), id("b"));
        assert_eq!(c.offer(&a, 1, Urgency::Regular, 0), Admit::Now(1));
        assert_eq!(c.offer(&b, 10, Urgency::Regular, 100), Admit::Queued);
        assert_eq!(c.offer(&a, 2, Urgency::Regular, 200), Admit::Queued);
        assert_eq!(c.poll(1_000), vec![(b.clone(), 10)]);
        // A token is back at 1 s; `a` is still queued, so a new `b`
        // waits behind it even with a token free later.
        assert_eq!(c.offer(&b, 11, Urgency::Regular, 1_500), Admit::Queued);
        assert_eq!(c.poll(2_000), vec![(a, 2)]);
        assert_eq!(c.poll(3_000), vec![(b, 11)]);
    }

    #[test]
    fn alerts_and_finishes_jump_the_queue_and_spend_tokens() {
        let mut c = Coalescer::new(Rate::new(1, 1), 0);
        let a = id("a");
        assert_eq!(c.offer(&a, 1, Urgency::Regular, 0), Admit::Now(1));
        assert_eq!(c.offer(&a, 2, Urgency::Regular, 0), Admit::Queued);
        // Posted at once; the queued regular update is superseded.
        assert_eq!(c.offer(&a, 3, Urgency::Finish, 0), Admit::Now(3));
        assert_eq!(c.queued(), 0);
        // The bucket is in debt by one token: the next regular update
        // waits two seconds.
        assert_eq!(c.offer(&a, 4, Urgency::Regular, 0), Admit::Queued);
        assert_eq!(c.next_due(), Some(2_000));
        assert_eq!(c.poll(1_999), Vec::new());
        assert_eq!(c.poll(2_000), vec![(a.clone(), 4)]);
        assert_eq!(c.offer(&a, 5, Urgency::Alert, 2_000), Admit::Now(5));
    }

    #[test]
    fn cancel_drops_the_queued_update() {
        let mut c = Coalescer::new(Rate::new(1, 1), 0);
        let a = id("a");
        c.offer(&a, 1, Urgency::Regular, 0);
        assert_eq!(c.offer(&a, 2, Urgency::Regular, 0), Admit::Queued);
        c.cancel(&a);
        assert_eq!(c.queued(), 0);
        assert_eq!(c.poll(10_000), Vec::new());
    }

    #[test]
    fn dismissed_ids_drop_regular_updates_until_an_alert() {
        let mut c = Coalescer::new(Rate::new(10, 10), 0);
        let a = id("a");
        c.offer(&a, 1, Urgency::Regular, 0);
        c.dismissed(&a);
        assert!(c.is_dismissed(&a));
        assert_eq!(c.offer(&a, 2, Urgency::Regular, 10), Admit::Dropped);
        assert_eq!(c.offer(&a, 3, Urgency::Alert, 20), Admit::Now(3));
        assert!(!c.is_dismissed(&a));
        assert_eq!(c.offer(&a, 4, Urgency::Regular, 30), Admit::Now(4));
        c.dismissed(&a);
        // A finish is a new show: it passes.
        assert_eq!(c.offer(&a, 5, Urgency::Finish, 40), Admit::Now(5));
        c.dismissed(&a);
        c.shown(&a, 50);
        assert_eq!(c.offer(&a, 6, Urgency::Regular, 60), Admit::Now(6));
    }

    #[test]
    fn the_bucket_refills_to_its_burst_only() {
        let mut c = Coalescer::new(Rate::new(4, 4), 0);
        let a = id("a");
        for value in 0..4 {
            assert_eq!(c.offer(&a, value, Urgency::Regular, 0), Admit::Now(value));
        }
        assert_eq!(c.offer(&a, 9, Urgency::Regular, 0), Admit::Queued);
        // Ten seconds idle refill four tokens, not forty.
        assert_eq!(c.poll(10_000), vec![(a.clone(), 9)]);
        for value in 0..3 {
            assert_eq!(
                c.offer(&a, value, Urgency::Regular, 10_000),
                Admit::Now(value)
            );
        }
        assert_eq!(c.offer(&a, 7, Urgency::Regular, 10_000), Admit::Queued);
        // A clock that goes back never adds tokens.
        assert_eq!(c.poll(5_000), Vec::new());
        assert_eq!(c.next_due(), Some(10_250));
    }

    #[test]
    fn debt_has_a_floor_of_one_burst() {
        let mut c = Coalescer::new(Rate::new(2, 2), 0);
        let a = id("a");
        // A storm of alerts and full shows: the bucket stops at -2 tokens.
        for value in 0..100 {
            assert_eq!(c.offer(&a, value, Urgency::Alert, 0), Admit::Now(value));
            c.shown(&a, 0);
        }
        assert_eq!(c.offer(&a, 7, Urgency::Regular, 0), Admit::Queued);
        // Three tokens to earn at 2 per second: 1.5 s, not 100 s.
        assert_eq!(c.next_due(), Some(1_500));
        assert_eq!(c.poll(1_500), vec![(a, 7)]);
    }

    #[test]
    fn a_new_rate_keeps_the_queue() {
        let mut c = Coalescer::new(Rate::new(10, 10), 0);
        let a = id("a");
        for value in 0..10 {
            assert_eq!(c.offer(&a, value, Urgency::Regular, 0), Admit::Now(value));
        }
        assert_eq!(c.offer(&a, 10, Urgency::Regular, 0), Admit::Queued);
        c.set_rate(Rate::new(1, 1), 0);
        assert_eq!(c.rate(), Rate::new(1, 1));
        assert_eq!(c.queued(), 1);
        assert_eq!(c.next_due(), Some(1_000));
        assert_eq!(c.poll(1_000), vec![(a, 10)]);
    }
}
