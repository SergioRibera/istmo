//! Which tab runs the schedule when the browser has no Web Locks: a
//! lease over `BroadcastChannel`. Pure: time and messages come in as
//! arguments, the host sends what it returns.
//!
//! The leader announces itself every [`BEAT_MS`]. A tab that has heard
//! no leader for [`EXPIRY_MS`] claims; a claim stands after
//! [`CLAIM_MS`] unless a leader beats or a lower tab id claims in the
//! meantime. Two leaders (a partition, a tab frozen past its expiry)
//! settle on their next beats: the higher id steps down. Best effort:
//! for a few seconds two tabs may both lead, so a delivery can repeat;
//! Web Locks, where present, have no such window.

/// How often the leader announces itself.
pub(crate) const BEAT_MS: i64 = 1_000;
/// Silence after which the leader counts as gone.
pub(crate) const EXPIRY_MS: i64 = 3_500;
/// How long a claim waits for objections.
pub(crate) const CLAIM_MS: i64 = 500;

/// A lease message between tabs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Message {
    /// `tab` leads.
    Beat(u32),
    /// `tab` wants to lead.
    Claim(u32),
    /// `tab` stopped leading (its page is going away).
    Resign(u32),
}

/// What the host does after a step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Step {
    /// Send this message.
    Send(Message),
    /// This tab leads now.
    Lead,
    /// This tab no longer leads.
    Follow,
}

/// One tab's view of the lease.
#[derive(Debug, Clone)]
pub(crate) struct Lease {
    me: u32,
    leading: bool,
    /// When a leader was last heard.
    heard_ms: Option<i64>,
    /// When this tab's claim started.
    claim_ms: Option<i64>,
    /// When this tab last beat.
    beat_ms: i64,
}

impl Lease {
    /// The lease of tab `me` (random, distinct per tab with high odds).
    pub(crate) const fn new(me: u32) -> Self {
        Self {
            me,
            leading: false,
            heard_ms: None,
            claim_ms: None,
            beat_ms: i64::MIN,
        }
    }

    #[cfg(test)]
    pub(crate) const fn leading(&self) -> bool {
        self.leading
    }

    /// A message from another tab at `now_ms`.
    pub(crate) const fn receive(&mut self, message: Message, now_ms: i64) -> Option<Step> {
        match message {
            Message::Beat(tab) if tab == self.me => None,
            Message::Beat(tab) => {
                if self.leading {
                    if tab < self.me {
                        self.leading = false;
                        self.heard_ms = Some(now_ms);
                        return Some(Step::Follow);
                    }
                    // The other one steps down when it hears this.
                    self.beat_ms = now_ms;
                    return Some(Step::Send(Message::Beat(self.me)));
                }
                self.heard_ms = Some(now_ms);
                self.claim_ms = None;
                None
            }
            Message::Claim(tab) => {
                if self.leading {
                    self.beat_ms = now_ms;
                    return Some(Step::Send(Message::Beat(self.me)));
                }
                if tab < self.me {
                    self.claim_ms = None;
                    // Give the lower claim its window before claiming again.
                    self.heard_ms = Some(now_ms);
                }
                None
            }
            Message::Resign(_) => {
                if !self.leading {
                    self.heard_ms = None;
                }
                None
            }
        }
    }

    /// Time passed: what to do at `now_ms`. Call it at least every
    /// [`CLAIM_MS`].
    pub(crate) fn tick(&mut self, now_ms: i64) -> Vec<Step> {
        if self.leading {
            if now_ms.saturating_sub(self.beat_ms) >= BEAT_MS {
                self.beat_ms = now_ms;
                return vec![Step::Send(Message::Beat(self.me))];
            }
            return Vec::new();
        }
        if let Some(since) = self.claim_ms {
            if now_ms.saturating_sub(since) >= CLAIM_MS {
                self.claim_ms = None;
                self.leading = true;
                self.beat_ms = now_ms;
                return vec![Step::Lead, Step::Send(Message::Beat(self.me))];
            }
            return Vec::new();
        }
        let silent = self
            .heard_ms
            .is_none_or(|at| now_ms.saturating_sub(at) >= EXPIRY_MS);
        if silent {
            self.claim_ms = Some(now_ms);
            return vec![Step::Send(Message::Claim(self.me))];
        }
        Vec::new()
    }

    /// The page goes away: the message to send, if it led.
    pub(crate) const fn resign(&mut self) -> Option<Message> {
        if self.leading {
            self.leading = false;
            Some(Message::Resign(self.me))
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Tabs on one channel: every sent message reaches the others.
    struct Bus(Vec<Lease>);

    impl Bus {
        fn run(&mut self, from: i64, to: i64) {
            let mut now = from;
            while now <= to {
                let mut sent = Vec::new();
                for (i, tab) in self.0.iter_mut().enumerate() {
                    for step in tab.tick(now) {
                        if let Step::Send(message) = step {
                            sent.push((i, message));
                        }
                    }
                }
                while let Some((from_tab, message)) = sent.pop() {
                    for (i, tab) in self.0.iter_mut().enumerate() {
                        if i != from_tab
                            && let Some(Step::Send(reply)) = tab.receive(message, now)
                        {
                            sent.push((i, reply));
                        }
                    }
                }
                now += 100;
            }
        }

        fn leaders(&self) -> Vec<u32> {
            self.0
                .iter()
                .filter(|t| t.leading())
                .map(|t| t.me)
                .collect()
        }
    }

    #[test]
    fn one_tab_leads_and_the_lowest_claim_wins() {
        let mut bus = Bus(vec![Lease::new(7), Lease::new(3), Lease::new(9)]);
        bus.run(0, 5_000);
        assert_eq!(bus.leaders(), [3]);
    }

    #[test]
    fn a_resigning_leader_is_replaced_quickly() {
        let mut bus = Bus(vec![Lease::new(3), Lease::new(7)]);
        bus.run(0, 3_000);
        assert_eq!(bus.leaders(), [3]);
        let resign = bus.0[0].resign().unwrap();
        bus.0.remove(0);
        bus.0[0].receive(resign, 3_000);
        bus.run(3_000, 4_000);
        assert_eq!(bus.leaders(), [7]);
    }

    #[test]
    fn a_silent_leader_expires() {
        let mut bus = Bus(vec![Lease::new(3), Lease::new(7)]);
        bus.run(0, 3_000);
        // Tab 3 is frozen (no beats) or closed without resigning.
        let frozen = bus.0.remove(0);
        bus.run(3_100, 3_100 + EXPIRY_MS + CLAIM_MS + 200);
        assert_eq!(bus.leaders(), [7]);
        // It comes back: the two settle on the lower id.
        bus.0.insert(0, frozen);
        bus.run(8_000, 10_000);
        assert_eq!(bus.leaders(), [3]);
    }

    #[test]
    fn a_tab_alone_leads_after_its_claim_window() {
        let mut lease = Lease::new(1);
        assert_eq!(lease.tick(0), [Step::Send(Message::Claim(1))]);
        assert_eq!(lease.tick(CLAIM_MS - 1), []);
        assert_eq!(
            lease.tick(CLAIM_MS),
            [Step::Lead, Step::Send(Message::Beat(1))]
        );
    }
}
