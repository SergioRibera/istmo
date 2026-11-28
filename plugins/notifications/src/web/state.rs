//! The web host's state machine: everything but the browser.
//!
//! Pure: time comes in as arguments, nothing touches JavaScript. The
//! host (`host.rs`) holds it under one lock, turns its [`Post`]s into
//! `showNotification` calls and its removals into `close()`, and feeds
//! clicks and closes back. Same rules as the desktop host: validation,
//! channel defaults, render fallbacks, coalescing at [`super::caps::RATE`],
//! the in-process scheduler, the delivered table, acks and events.
//!
//! Several tabs share one store. Only the leader tab runs the schedule
//! ([`State::set_leader`]); every tab keeps its engine as a mirror of
//! the store, writes the records it changed ([`State::take_writes`],
//! one record per entry, never the whole set) and reloads when another
//! tab wrote ([`State::replace_entries`], local changes the reload does
//! not include yet win).

use std::collections::{HashMap, HashSet, VecDeque};

use jiff::tz::TimeZone;

use crate::coalesce::{Admit, Coalescer, Urgency};
use crate::render::{RenderLimits, Rendered, Resolved, render};
use crate::schedule::{Due, Engine, Entry, Store, StoreError};
use crate::{
    AckToken, ActionId, ChannelId, ChannelSpec, DeliveredNotification, DismissReason, Dismissal,
    Failure, Notification, NotificationError, NotificationEvent, NotificationId,
    NotificationResponse, PendingNotification, ResponseAction, ScheduledInfo, Trigger,
};

/// Events kept for the first subscriber.
const BACKLOG: usize = 64;
/// Depth of each subscriber's channel.
const SUBSCRIBER_DEPTH: usize = 256;
/// Dismissed notifications kept (for an alerting update to show them
/// again).
const MAX_DISMISSED: usize = 256;

/// How a post relates to what is on screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShowMode {
    /// Nothing with this id is shown.
    New,
    /// Replaces the one with this id (same tag).
    Replace {
        /// Alert again (`renotify`); `false` = silent update.
        alert: bool,
    },
}

/// One notification to hand to the browser.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Post {
    /// Validated, normalised, channel defaults merged.
    pub(crate) notification: Notification,
    pub(crate) rendered: Rendered,
    pub(crate) mode: ShowMode,
    /// Images must be resolved again (otherwise the URLs of the previous
    /// post are reused).
    pub(crate) fresh_images: bool,
}

/// A shown notification.
#[derive(Debug)]
struct Live {
    notification: Notification,
    rendered: Rendered,
    images_stale: bool,
    delivered_ms: i64,
    dismissed: bool,
    expires_ms: Option<i64>,
}

/// What a click means.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Activated {
    /// Publish it.
    Respond(NotificationResponse),
    /// Hide `notification`; it comes back after the delay
    /// ([`State::snooze`]).
    Snooze(Box<Notification>, u64),
}

/// What is due at a tick.
#[derive(Debug, Default)]
pub(crate) struct Tick {
    /// Scheduled deliveries (show them with [`State::prepare_show`]).
    pub(crate) due: Vec<Due>,
    /// Queued updates ready to post.
    pub(crate) queued: Vec<Post>,
    /// Ids whose host-run timeout passed.
    pub(crate) expired: Vec<NotificationId>,
}

#[derive(Debug, Default)]
struct EventHub {
    backlog: VecDeque<NotificationEvent>,
    subscribers: Vec<flume::Sender<NotificationEvent>>,
    subscribed: bool,
}

impl EventHub {
    fn emit(&mut self, event: NotificationEvent) {
        if !self.subscribed {
            if self.backlog.len() == BACKLOG {
                self.backlog.pop_front();
            }
            self.backlog.push_back(event);
            return;
        }
        self.subscribers
            .retain(|tx| match tx.try_send(event.clone()) {
                Ok(()) => true,
                Err(flume::TrySendError::Full(_)) => {
                    tracing::warn!(
                        "notifications: an event subscriber is not reading; event dropped"
                    );
                    true
                }
                Err(flume::TrySendError::Disconnected(_)) => false,
            });
    }

    fn subscribe(&mut self) -> flume::Receiver<NotificationEvent> {
        let (tx, rx) = flume::bounded(SUBSCRIBER_DEPTH);
        if !self.subscribed {
            self.subscribed = true;
            for event in self.backlog.drain(..) {
                let _queued = tx.try_send(event);
            }
        }
        self.subscribers.push(tx);
        rx
    }
}

/// The host's state.
#[derive(Debug)]
pub(crate) struct State {
    table: HashMap<NotificationId, Live>,
    dismissed: VecDeque<NotificationId>,
    coalescer: Coalescer<()>,
    engine: Engine,
    channels: HashMap<ChannelId, ChannelSpec>,
    acks: HashMap<u64, NotificationId>,
    next_ack: u64,
    limits: RenderLimits,
    events: EventHub,
    /// Whether this tab runs the schedule: delivers due entries and
    /// applies [`crate::MissedPolicy`].
    leader: bool,
    /// Local engine edits, counted.
    edits: u64,
    /// The edit number of each id's latest local change, kept until a
    /// reload that started after it has been applied.
    touched: HashMap<NotificationId, u64>,
    /// Ids whose stored record must be rewritten or deleted.
    unwritten: HashSet<NotificationId>,
}

/// One stored record to write: the entry in the core store format (a
/// one-entry store), or `None` to delete it.
pub(crate) type Write = (NotificationId, Result<Option<Vec<u8>>, StoreError>);

/// Notifications in a known channel get the channel's defaults.
fn merge_channel(
    n: &mut Notification,
    channels: &HashMap<ChannelId, ChannelSpec>,
) -> Result<(), NotificationError> {
    let Some(id) = &n.channel else {
        return Ok(());
    };
    let Some(channel) = channels.get(id) else {
        return Err(NotificationError::UnknownChannel(id.value.clone()));
    };
    if n.interruption.is_none() {
        n.interruption = Some(channel.importance);
    }
    if n.sound.is_none() {
        n.sound.clone_from(&channel.sound);
    }
    if n.lock_screen.is_none() {
        n.lock_screen = channel.lock_screen;
    }
    Ok(())
}

/// The action id of a rendered key (`<id>#<choice>` for choices).
fn action_of(key: &str) -> &str {
    key.split_once('#').map_or(key, |(action, _)| action)
}

impl State {
    pub(crate) fn new(limits: RenderLimits, channels: Vec<ChannelSpec>, now_ms: i64) -> Self {
        Self {
            table: HashMap::new(),
            dismissed: VecDeque::new(),
            coalescer: Coalescer::new(super::caps::RATE, now_ms),
            engine: Engine::default(),
            channels: channels.into_iter().map(|c| (c.id.clone(), c)).collect(),
            acks: HashMap::new(),
            next_ack: 1,
            limits,
            events: EventHub::default(),
            leader: true,
            edits: 0,
            touched: HashMap::new(),
            unwritten: HashSet::new(),
        }
    }

    /// The browser's limits changed (service worker found or lost).
    pub(crate) fn set_limits(&mut self, limits: RenderLimits) {
        if self.limits != limits {
            self.limits = limits;
            for live in self.table.values_mut() {
                live.rendered = render(&live.notification, &self.limits);
            }
        }
    }

    pub(crate) fn emit(&mut self, event: NotificationEvent) {
        self.events.emit(event);
    }

    pub(crate) fn subscribe(&mut self) -> flume::Receiver<NotificationEvent> {
        self.events.subscribe()
    }

    pub(crate) fn failed(&mut self, id: Option<&NotificationId>, message: String) {
        self.emit(NotificationEvent::Failed(Failure {
            id: id.cloned(),
            message,
        }));
    }

    fn post_of(&self, notification: Notification, mode: ShowMode) -> Post {
        let current = self
            .table
            .get(&notification.id)
            .filter(|live| !live.dismissed);
        Post {
            rendered: render(&notification, &self.limits),
            fresh_images: current.is_none_or(|live| live.images_stale),
            notification,
            mode,
        }
    }

    /// A full show: validated, normalised, channel merged.
    pub(crate) fn prepare_show(
        &mut self,
        mut notification: Notification,
        now_ms: i64,
    ) -> Result<Post, NotificationError> {
        notification.validate()?;
        notification.normalize();
        merge_channel(&mut notification, &self.channels)?;
        let shown = self
            .table
            .get(&notification.id)
            .is_some_and(|live| !live.dismissed);
        self.coalescer.shown(&notification.id, now_ms);
        let mode = if shown {
            ShowMode::Replace { alert: true }
        } else {
            ShowMode::New
        };
        Ok(self.post_of(notification, mode))
    }

    /// When `id` was first shown, for a post of it now.
    pub(crate) fn delivered_ms(&self, id: &NotificationId, mode: ShowMode, now_ms: i64) -> i64 {
        self.table
            .get(id)
            .filter(|live| !live.dismissed && mode != ShowMode::New)
            .map_or(now_ms, |live| live.delivered_ms)
    }

    /// `post` reached the browser at `now_ms`.
    pub(crate) fn posted(&mut self, post: Post, delivered_ms: i64, now_ms: i64) {
        let id = post.notification.id.clone();
        let expires_ms = post
            .notification
            .timeout_ms
            .map(|ms| now_ms.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)));
        if let Some(at) = self.dismissed.iter().position(|seen| *seen == id) {
            self.dismissed.remove(at);
        }
        self.table.insert(
            id,
            Live {
                notification: post.notification,
                rendered: post.rendered,
                images_stale: false,
                delivered_ms,
                dismissed: false,
                expires_ms,
            },
        );
    }

    /// Changes the shown `id`; the post to send now, if the coalescer
    /// lets it go.
    pub(crate) fn change(
        &mut self,
        id: &NotificationId,
        urgency: Urgency,
        touches_images: bool,
        change: impl FnOnce(&mut Notification) -> Result<(), NotificationError>,
        now_ms: i64,
    ) -> Result<Option<Post>, NotificationError> {
        let live = self
            .table
            .get_mut(id)
            .ok_or_else(|| NotificationError::NotFound(id.value.clone()))?;
        let before = live.notification.clone();
        change(&mut live.notification)?;
        if let Err(err) = live.notification.validate() {
            live.notification = before;
            return Err(err.into());
        }
        live.notification.normalize();
        live.images_stale |= touches_images;
        let was_dismissed = live.dismissed;
        Ok(match self.coalescer.offer(id, (), urgency, now_ms) {
            Admit::Dropped | Admit::Queued => None,
            Admit::Now(()) => {
                let notification = self.table[id].notification.clone();
                let mode = if was_dismissed {
                    ShowMode::New
                } else {
                    ShowMode::Replace {
                        alert: urgency != Urgency::Regular,
                    }
                };
                Some(self.post_of(notification, mode))
            }
        })
    }

    /// Takes `ids` out of the table and the schedule; the ones that were
    /// on screen.
    pub(crate) fn remove(&mut self, ids: &[NotificationId]) -> Vec<NotificationId> {
        let mut shown = Vec::with_capacity(ids.len());
        for id in ids {
            self.coalescer.cancel(id);
            self.engine.remove(id);
            // Also when this mirror does not hold it: another tab may
            // have scheduled it since the last reload.
            self.touch(id);
            if let Some(at) = self.dismissed.iter().position(|seen| seen == id) {
                self.dismissed.remove(at);
            }
            if let Some(live) = self.table.remove(id)
                && !live.dismissed
            {
                shown.push(id.clone());
            }
            self.acks.retain(|_, acked| acked != id);
        }
        shown
    }

    /// Ids of `group`, shown or scheduled.
    pub(crate) fn group_ids(&self, group: &crate::GroupId) -> Vec<NotificationId> {
        let in_group = |n: &Notification| n.group.as_ref().is_some_and(|g| &g.id == group);
        self.table
            .values()
            .map(|live| &live.notification)
            .chain(self.engine.entries().into_iter().map(|e| &e.notification))
            .filter(|n| in_group(n))
            .map(|n| n.id.clone())
            .collect()
    }

    /// Every id, shown or scheduled.
    pub(crate) fn all_ids(&self) -> Vec<NotificationId> {
        self.table
            .keys()
            .cloned()
            .chain(
                self.engine
                    .entries()
                    .into_iter()
                    .map(|e| e.notification.id.clone()),
            )
            .collect()
    }

    /// Schedules `notification`.
    pub(crate) fn schedule(
        &mut self,
        notification: Notification,
        trigger: Trigger,
        now_ms: i64,
    ) -> Result<ScheduledInfo, NotificationError> {
        notification.validate()?;
        trigger.validate()?;
        if let Some(channel) = &notification.channel
            && !self.channels.contains_key(channel)
        {
            return Err(NotificationError::UnknownChannel(channel.value.clone()));
        }
        // A trigger that never fires (a rule past its `until`) is refused:
        // the engine does not store it.
        let dead = trigger.never_fires();
        let id = notification.id.clone();
        let info = self.engine.add(notification, trigger, now_ms);
        if info.next_fire_ms.is_none() {
            return Err(dead.into());
        }
        self.touch(&id);
        Ok(info)
    }

    /// Hides nothing (the caller removed it): schedules `notification`
    /// again after `ms` and reports the snooze.
    pub(crate) fn snooze(&mut self, notification: Notification, ms: u64, now_ms: i64) {
        let id = notification.id.clone();
        self.engine.add(notification, Trigger::After(ms), now_ms);
        self.touch(&id);
        self.emit(NotificationEvent::Snoozed(id));
    }

    /// Takes what is due at `now_ms`. Only the leader takes scheduled
    /// entries (and applies their missed policy); every tab runs its
    /// coalescer and timeouts.
    pub(crate) fn tick(&mut self, now_ms: i64) -> Tick {
        let due = if self.leader {
            let ids: Vec<NotificationId> = self
                .engine
                .snapshot()
                .iter()
                .filter(|entry| entry.next_fire_ms.is_some_and(|at| at <= now_ms))
                .map(|entry| entry.notification.id.clone())
                .collect();
            // Delivered, moved to their next time, or dropped by their
            // missed policy: each record changes.
            for id in &ids {
                self.touch(id);
            }
            self.engine.due(now_ms)
        } else {
            Vec::new()
        };
        let queued_ids: Vec<NotificationId> = self
            .coalescer
            .poll(now_ms)
            .into_iter()
            .map(|(id, ())| id)
            .collect();
        let mut queued = Vec::with_capacity(queued_ids.len());
        for id in queued_ids {
            if let Some(live) = self.table.get(&id).filter(|live| !live.dismissed) {
                let notification = live.notification.clone();
                queued.push(self.post_of(notification, ShowMode::Replace { alert: false }));
            }
        }
        let expired = self
            .table
            .iter()
            .filter(|(_, live)| !live.dismissed && live.expires_ms.is_some_and(|at| at <= now_ms))
            .map(|(id, _)| id.clone())
            .collect();
        Tick {
            due,
            queued,
            expired,
        }
    }

    /// The next deadline of everything the host waits for.
    pub(crate) fn next_deadline(&self) -> Option<i64> {
        let expiry = self.table.values().filter_map(|live| live.expires_ms).min();
        [
            self.engine.next_deadline().filter(|_| self.leader),
            self.coalescer.next_due(),
            expiry,
        ]
        .into_iter()
        .flatten()
        .min()
    }

    /// A notification an earlier page showed, still on screen (rebuilt
    /// from its record). Ids the table knows already are kept.
    pub(crate) fn adopt(&mut self, notification: Notification, delivered_ms: i64) {
        if self.table.contains_key(&notification.id) {
            return;
        }
        let expires_ms = notification
            .timeout_ms
            .map(|ms| delivered_ms.saturating_add(i64::try_from(ms).unwrap_or(i64::MAX)));
        self.table.insert(
            notification.id.clone(),
            Live {
                rendered: render(&notification, &self.limits),
                notification,
                images_stale: true,
                delivered_ms,
                dismissed: false,
                expires_ms,
            },
        );
    }

    /// A click on `id`: the body (`key = None`) or a button. The browser
    /// closed the notification; it stays in the table as dismissed (an
    /// alerting update shows it again), without a dismissal event.
    /// `record` stands in when the table does not know `id`.
    pub(crate) fn activated(
        &mut self,
        id: &NotificationId,
        key: Option<&str>,
        launched_app: bool,
        record: Option<Notification>,
    ) -> Option<Activated> {
        if !self.table.contains_key(id)
            && let Some(record) = record.filter(|n| n.id == *id)
        {
            self.adopt(record, 0);
        }
        let Some(live) = self.table.get(id) else {
            // Neither the table nor a record: an id alone.
            return Some(Activated::Respond(NotificationResponse {
                id: id.clone(),
                action: match key {
                    None => ResponseAction::Open,
                    Some(ActionId::MORE) => ResponseAction::More,
                    Some(key) => ResponseAction::Button(ActionId {
                        value: action_of(key).to_owned(),
                    }),
                },
                reply: None,
                input_requested: false,
                user_data: None,
                url: None,
                launched_app,
                overflow_actions: Vec::new(),
                ack: None,
            }));
        };
        let background = key.is_some_and(|key| {
            live.notification
                .actions
                .iter()
                .any(|a| a.id.as_str() == action_of(key) && !a.opens_app)
        });
        let resolved = live
            .rendered
            .respond(&live.notification, key, None, launched_app);
        let notification = live.notification.clone();
        let out = match resolved? {
            Resolved::Snooze(ms) => return Some(Activated::Snooze(Box::new(notification), ms)),
            Resolved::Respond(mut response) => {
                if background {
                    let value = self.next_ack;
                    self.next_ack += 1;
                    self.acks.insert(value, response.id.clone());
                    response.ack = Some(AckToken { value });
                }
                Activated::Respond(response)
            }
        };
        self.mark_gone(id);
        Some(out)
    }

    /// `id` left the screen: kept as dismissed.
    fn mark_gone(&mut self, id: &NotificationId) -> bool {
        let Some(live) = self.table.get_mut(id) else {
            return false;
        };
        if live.dismissed {
            return false;
        }
        live.dismissed = true;
        live.images_stale = true;
        live.expires_ms = None;
        self.coalescer.dismissed(id);
        self.dismissed.push_back(id.clone());
        while self.dismissed.len() > MAX_DISMISSED {
            if let Some(old) = self.dismissed.pop_front() {
                self.table.remove(&old);
                self.coalescer.cancel(&old);
            }
        }
        true
    }

    /// The user closed `id`. `record` stands in for an unknown id.
    pub(crate) fn dismissed(
        &mut self,
        id: &NotificationId,
        reason: DismissReason,
        record: Option<Notification>,
    ) {
        if !self.table.contains_key(id)
            && let Some(record) = record.filter(|n| n.id == *id)
        {
            self.adopt(record, 0);
        }
        if self.mark_gone(id) {
            self.emit(NotificationEvent::Dismissed(Dismissal {
                id: id.clone(),
                reason,
            }));
        }
    }

    /// A background response is done.
    pub(crate) fn complete(&mut self, ack: AckToken) -> bool {
        self.acks.remove(&ack.value).is_some()
    }

    /// Scheduled entries, soonest first.
    pub(crate) fn pending(&self) -> Vec<PendingNotification> {
        self.engine.pending()
    }

    /// Ids on screen as the table knows them.
    pub(crate) fn delivered(&self) -> Vec<DeliveredNotification> {
        let mut out: Vec<DeliveredNotification> = self
            .table
            .values()
            .filter(|live| !live.dismissed)
            .map(|live| delivered_of(&live.notification, live.delivered_ms))
            .collect();
        out.sort_by(|a, b| (a.delivered_ms, &a.id).cmp(&(b.delivered_ms, &b.id)));
        out
    }

    /// Creates or renames channels (existing ones keep their settings).
    pub(crate) fn set_channels(&mut self, channels: Vec<ChannelSpec>) {
        for channel in channels {
            match self.channels.get_mut(&channel.id) {
                Some(existing) => {
                    existing.name = channel.name;
                    existing.description = channel.description;
                }
                None => {
                    self.channels.insert(channel.id.clone(), channel);
                }
            }
        }
    }

    pub(crate) fn delete_channel(&mut self, id: &ChannelId) {
        self.channels.remove(id);
    }

    /// Records a local change of `id`'s scheduled entry.
    fn touch(&mut self, id: &NotificationId) {
        self.edits += 1;
        self.touched.insert(id.clone(), self.edits);
        self.unwritten.insert(id.clone());
    }

    /// Whether this tab runs the schedule.
    pub(crate) const fn is_leader(&self) -> bool {
        self.leader
    }

    /// Makes this tab the one running the schedule, or a follower.
    pub(crate) const fn set_leader(&mut self, leader: bool) {
        self.leader = leader;
    }

    /// The mark of a reload starting now; hand it back to
    /// [`Self::replace_entries`].
    pub(crate) const fn edit_mark(&self) -> u64 {
        self.edits
    }

    /// The device's zone is `local` now (the leader checks it on each
    /// wake): floating calendar rules move to it and their records are
    /// rewritten. Whether anything changed.
    pub(crate) fn set_zone(&mut self, local: TimeZone, now_ms: i64) -> bool {
        if !self.leader || !self.engine.set_local(local, now_ms) {
            return false;
        }
        let ids: Vec<NotificationId> = self
            .engine
            .snapshot()
            .iter()
            .map(|entry| entry.notification.id.clone())
            .collect();
        for id in &ids {
            self.touch(id);
        }
        true
    }

    /// Replaces the schedule with `stored`, read from the store by a
    /// reload that started at `mark`. Ids changed here after `mark` keep
    /// their local state (the read may predate their write).
    pub(crate) fn replace_entries(&mut self, mark: u64, stored: Vec<Entry>) {
        let local = self.engine.local().clone();
        let newer: Vec<&NotificationId> = self
            .touched
            .iter()
            .filter(|(_, at)| **at > mark)
            .map(|(id, _)| id)
            .collect();
        let kept: Vec<Entry> = self
            .engine
            .snapshot()
            .into_iter()
            .filter(|entry| newer.contains(&&entry.notification.id))
            .map(std::sync::Arc::unwrap_or_clone)
            .collect();
        let mut engine = Engine::with_entries(local.clone(), stored);
        for id in newer {
            engine.remove(id);
        }
        engine.merge(Engine::with_entries(local, kept));
        self.engine = engine;
        self.touched.retain(|_, at| *at > mark);
    }

    /// The records to write since the last call: one per changed id.
    pub(crate) fn take_writes(&mut self) -> Vec<Write> {
        if self.unwritten.is_empty() {
            return Vec::new();
        }
        let entries: HashMap<NotificationId, std::sync::Arc<Entry>> = self
            .engine
            .snapshot()
            .into_iter()
            .map(|entry| (entry.notification.id.clone(), entry))
            .collect();
        let mut out: Vec<Write> = self
            .unwritten
            .drain()
            .map(|id| {
                let record = entries
                    .get(&id)
                    .map(|entry| Store::encode(std::iter::once(entry.as_ref())))
                    .transpose();
                (id, record)
            })
            .collect();
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }
}

/// The entries of stored records (each a one-entry store); unreadable
/// records are skipped and reported.
pub(crate) fn decode_records<'a>(
    records: impl IntoIterator<Item = &'a [u8]>,
) -> (Vec<Entry>, Vec<StoreError>) {
    let mut entries = Vec::new();
    let mut errors = Vec::new();
    for record in records {
        match Store::decode(record) {
            Ok(decoded) => entries.extend(decoded),
            Err(err) => errors.push(err),
        }
    }
    (entries, errors)
}

/// The listing entry of `n`.
pub(crate) fn delivered_of(n: &Notification, delivered_ms: i64) -> DeliveredNotification {
    DeliveredNotification {
        id: n.id.clone(),
        title: Some(n.title.clone()),
        body: n.body.clone(),
        group: n.group.as_ref().map(|g| g.id.clone()),
        delivered_ms: Some(delivered_ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, CalendarRule, MissedPolicy, NotificationPatch, Progress};

    fn id(s: &str) -> NotificationId {
        NotificationId::try_from(s).unwrap()
    }

    fn limits(max_actions: u32) -> RenderLimits {
        RenderLimits {
            max_actions,
            ..RenderLimits::default()
        }
    }

    fn state() -> State {
        State::new(limits(2), Vec::new(), 0)
    }

    fn show(s: &mut State, n: Notification, now: i64) -> Post {
        let post = s.prepare_show(n, now).unwrap();
        let delivered = s.delivered_ms(&post.notification.id, post.mode, now);
        s.posted(post.clone(), delivered, now);
        post
    }

    #[test]
    fn a_second_show_replaces_and_alerts() {
        let mut s = state();
        let first = show(&mut s, Notification::new("a", "one").unwrap(), 0);
        assert_eq!(first.mode, ShowMode::New);
        assert!(first.fresh_images);
        let second = show(&mut s, Notification::new("a", "two").unwrap(), 5);
        assert_eq!(second.mode, ShowMode::Replace { alert: true });
        assert_eq!(s.delivered()[0].delivered_ms, Some(0));
    }

    #[test]
    fn progress_updates_coalesce_to_the_web_rate() {
        let mut s = state();
        show(&mut s, Notification::new("dl", "Download").unwrap(), 0);
        let mut posted = 0;
        for step in 0..20u8 {
            let progress = Progress::fraction(f32::from(step) / 20.0);
            let post = s
                .change(
                    &id("dl"),
                    Urgency::Regular,
                    false,
                    |n| {
                        n.progress = Some(progress);
                        Ok(())
                    },
                    0,
                )
                .unwrap();
            if let Some(post) = post {
                assert_eq!(post.mode, ShowMode::Replace { alert: false });
                assert!(!post.fresh_images);
                posted += 1;
            }
        }
        // The bucket held 4, the show spent one.
        assert_eq!(posted, 3);
        let due = s.next_deadline().unwrap();
        let tick = s.tick(due);
        assert_eq!(tick.queued.len(), 1);
        assert_eq!(
            tick.queued[0].rendered.body.as_deref(),
            Some("95%"),
            "the latest value wins"
        );
    }

    #[test]
    fn updates_of_unknown_ids_fail() {
        let mut s = state();
        let err = s
            .change(&id("x"), Urgency::Regular, false, |_| Ok(()), 0)
            .unwrap_err();
        assert!(matches!(err, NotificationError::NotFound(_)));
        let patch = NotificationPatch::default();
        assert!(patch.validate().is_ok());
    }

    #[test]
    fn clicks_resolve_keys_and_hand_out_acks() {
        let mut s = state();
        let n = Notification::new("m", "Msg")
            .unwrap()
            .with_user_data("chat-7")
            .with_action(Action::button("read", "Read").unwrap().background())
            .with_action(Action::button("open", "Open").unwrap());
        show(&mut s, n, 0);
        let Some(Activated::Respond(r)) = s.activated(&id("m"), Some("read"), false, None) else {
            panic!("no response");
        };
        assert_eq!(
            r.action,
            ResponseAction::Button(ActionId::try_from("read").unwrap())
        );
        assert_eq!(r.user_data.as_deref(), Some("chat-7"));
        let ack = r.ack.unwrap();
        assert!(s.complete(ack));
        assert!(!s.complete(ack));
        // Clicked: gone from the delivered list, no dismissal event.
        assert_eq!(s.delivered(), Vec::new());
        let events = s.subscribe();
        assert!(events.try_recv().is_err());
    }

    #[test]
    fn records_answer_for_notifications_of_an_earlier_page() {
        let mut s = state();
        let record = Notification::new("old", "Old")
            .unwrap()
            .with_url("app://x")
            .with_action(Action::reply("reply", "Reply").unwrap());
        let Some(Activated::Respond(r)) =
            s.activated(&id("old"), Some("reply"), true, Some(record))
        else {
            panic!("no response");
        };
        assert!(r.input_requested, "reply is simulated on the web");
        assert!(r.launched_app);
        assert_eq!(r.url.as_deref(), Some("app://x"));
        // Without a record: the id and the key only.
        let Some(Activated::Respond(bare)) = s.activated(&id("gone"), None, false, None) else {
            panic!("no response");
        };
        assert_eq!(bare.action, ResponseAction::Open);
    }

    #[test]
    fn snoozes_come_back_after_their_delay() {
        let mut s = state();
        let n = Notification::new("s", "Later").unwrap().with_action(
            Action::snooze("snooze", "10 min", std::time::Duration::from_secs(600)).unwrap(),
        );
        show(&mut s, n, 0);
        let Some(Activated::Snooze(n, ms)) = s.activated(&id("s"), Some("snooze"), false, None)
        else {
            panic!("no snooze");
        };
        assert_eq!(ms, 600_000);
        assert_eq!(s.remove(&[id("s")]), vec![id("s")]);
        s.snooze(*n, ms, 1_000);
        assert_eq!(s.next_deadline(), Some(601_000));
        let tick = s.tick(601_000);
        assert_eq!(tick.due.len(), 1);
    }

    #[test]
    fn closes_are_reported_once_and_drop_regular_updates() {
        let mut s = state();
        show(&mut s, Notification::new("c", "t").unwrap(), 0);
        let events = s.subscribe();
        s.dismissed(&id("c"), DismissReason::User, None);
        s.dismissed(&id("c"), DismissReason::User, None);
        assert!(matches!(
            events.try_recv(),
            Ok(NotificationEvent::Dismissed(_))
        ));
        assert!(events.try_recv().is_err());
        let regular = s
            .change(&id("c"), Urgency::Regular, false, |_| Ok(()), 10_000)
            .unwrap();
        assert_eq!(regular, None);
        let alert = s
            .change(&id("c"), Urgency::Alert, false, |_| Ok(()), 10_000)
            .unwrap()
            .unwrap();
        assert_eq!(alert.mode, ShowMode::New);
        assert!(alert.fresh_images);
    }

    #[test]
    fn host_run_timeouts_expire() {
        let mut s = state();
        show(
            &mut s,
            Notification::new("t", "t")
                .unwrap()
                .with_timeout(std::time::Duration::from_secs(5)),
            1_000,
        );
        assert_eq!(s.next_deadline(), Some(6_000));
        assert_eq!(s.tick(6_000).expired, vec![id("t")]);
    }

    #[test]
    fn schedules_persist_in_the_core_store_format_and_missed_ones_follow_policy() {
        let mut s = state();
        assert_eq!(s.take_writes().len(), 0);
        s.schedule(Notification::new("a", "t").unwrap(), Trigger::At(10_000), 0)
            .unwrap();
        s.schedule(
            Notification::new("b", "t")
                .unwrap()
                .with_missed(MissedPolicy::Drop),
            Trigger::At(10_000),
            0,
        )
        .unwrap();
        let mut db = Records::default();
        db.apply(s.take_writes());
        assert_eq!(db.0.len(), 2);
        assert!(
            db.0.values()
                .all(|bytes| bytes[0] == crate::schedule::STORE_FORMAT)
        );
        assert_eq!(s.take_writes().len(), 0, "written once");
        // A later page, two minutes past the time.
        let mut later = state();
        db.reload(&mut later);
        assert_eq!(later.pending().len(), 2);
        let tick = later.tick(130_000);
        let fired: Vec<_> = tick
            .due
            .iter()
            .map(|d| (d.notification.id.value.as_str(), d.late))
            .collect();
        assert_eq!(fired, [("a", true)]);
        // Both one-shots leave the store, the dropped one too.
        db.apply(later.take_writes());
        assert_eq!(db.0.len(), 0);
    }

    /// The store of a test: one record per id.
    #[derive(Default)]
    struct Records(std::collections::BTreeMap<NotificationId, Vec<u8>>);

    impl Records {
        fn apply(&mut self, writes: Vec<Write>) {
            for (id, record) in writes {
                match record.unwrap() {
                    Some(bytes) => self.0.insert(id, bytes),
                    None => self.0.remove(&id),
                };
            }
        }

        fn reload(&self, s: &mut State) {
            let mark = s.edit_mark();
            let (entries, errors) = decode_records(self.0.values().map(Vec::as_slice));
            assert_eq!(errors.len(), 0);
            s.replace_entries(mark, entries);
        }
    }

    fn ids_pending(s: &State) -> Vec<String> {
        s.pending()
            .into_iter()
            .map(|p| p.notification.id.value)
            .collect()
    }

    #[test]
    fn only_the_leader_delivers_and_followers_write_their_own_records() {
        let mut db = Records::default();
        let mut leader = state();
        let mut follower = state();
        follower.set_leader(false);
        follower
            .schedule(Notification::new("a", "t").unwrap(), Trigger::At(10_000), 0)
            .unwrap();
        // The follower waits for nothing scheduled and delivers nothing.
        assert_eq!(follower.next_deadline(), None);
        assert_eq!(follower.tick(20_000).due.len(), 0);
        assert_eq!(ids_pending(&follower), ["a"]);
        db.apply(follower.take_writes());
        // The leader hears about it, reloads and delivers it once.
        db.reload(&mut leader);
        assert_eq!(leader.next_deadline(), Some(10_000));
        let due: Vec<_> = leader.tick(10_000).due;
        assert_eq!(due.len(), 1);
        db.apply(leader.take_writes());
        // No tab resurrects the delivered one-shot.
        db.reload(&mut follower);
        assert_eq!(ids_pending(&follower).len(), 0);
        assert_eq!(follower.take_writes().len(), 0);
        assert_eq!(db.0.len(), 0);
    }

    #[test]
    fn a_follower_schedule_survives_another_tabs_write() {
        let mut db = Records::default();
        let mut a = state();
        let mut b = state();
        b.set_leader(false);
        a.schedule(Notification::new("x", "t").unwrap(), Trigger::At(50_000), 0)
            .unwrap();
        b.schedule(Notification::new("y", "t").unwrap(), Trigger::At(60_000), 0)
            .unwrap();
        // Whole-set overwrites lost one of the two; per-entry records keep both.
        db.apply(a.take_writes());
        db.apply(b.take_writes());
        db.reload(&mut a);
        db.reload(&mut b);
        assert_eq!(ids_pending(&a), ["x", "y"]);
        assert_eq!(ids_pending(&b), ["x", "y"]);
    }

    #[test]
    fn a_reload_older_than_a_local_change_keeps_the_change() {
        let mut db = Records::default();
        let mut s = state();
        s.schedule(
            Notification::new("old", "t").unwrap(),
            Trigger::At(9_000),
            0,
        )
        .unwrap();
        db.apply(s.take_writes());
        // A reload starts, then `new` is scheduled and `old` cancelled
        // before its read lands.
        let mark = s.edit_mark();
        let (stored, _) = decode_records(db.0.values().map(Vec::as_slice));
        s.schedule(
            Notification::new("new", "t").unwrap(),
            Trigger::At(8_000),
            0,
        )
        .unwrap();
        s.remove(&[id("old")]);
        s.replace_entries(mark, stored);
        assert_eq!(ids_pending(&s), ["new"]);
        // A later reload that includes the writes settles it.
        db.apply(s.take_writes());
        db.reload(&mut s);
        assert_eq!(ids_pending(&s), ["new"]);
    }

    #[test]
    fn a_follower_cancel_deletes_entries_its_mirror_misses() {
        let mut db = Records::default();
        let mut a = state();
        a.schedule(Notification::new("z", "t").unwrap(), Trigger::At(9_000), 0)
            .unwrap();
        db.apply(a.take_writes());
        let mut b = state();
        b.set_leader(false);
        b.remove(&[id("z")]);
        db.apply(b.take_writes());
        assert_eq!(db.0.len(), 0);
    }

    #[test]
    fn only_the_leader_moves_floating_rules_to_a_new_zone() {
        let mut s = state();
        s.set_leader(false);
        let rule = Trigger::Calendar(CalendarRule::daily(8, 0));
        s.schedule(Notification::new("c", "t").unwrap(), rule, 0)
            .unwrap();
        let _ = s.take_writes();
        // A fixed offset never equals the system's named zone.
        let zone = TimeZone::fixed(jiff::tz::offset(9));
        assert!(!s.set_zone(zone.clone(), 0));
        s.set_leader(true);
        assert!(s.set_zone(zone.clone(), 0));
        assert_eq!(s.take_writes().len(), 1);
        assert!(!s.set_zone(zone, 0), "same zone again: nothing moves");
    }

    #[test]
    fn triggers_that_never_fire_are_refused() {
        let mut s = state();
        let rule = CalendarRule::daily(8, 0)
            .in_zone("UTC")
            .until(std::time::UNIX_EPOCH);
        let err = s
            .schedule(
                Notification::new("n", "t").unwrap(),
                Trigger::Calendar(rule),
                86_400_000,
            )
            .unwrap_err();
        assert!(matches!(err, NotificationError::Invalid(_)));
        assert_eq!(s.pending(), Vec::new());
    }

    #[test]
    fn unknown_channels_fail_and_known_ones_merge() {
        let mut s = state();
        let n = Notification::new("n", "t")
            .unwrap()
            .with_channel("chat")
            .unwrap();
        assert!(matches!(
            s.prepare_show(n.clone(), 0),
            Err(NotificationError::UnknownChannel(_))
        ));
        s.set_channels(vec![ChannelSpec {
            id: ChannelId::try_from("chat").unwrap(),
            name: "Chat".to_owned(),
            description: None,
            importance: crate::Interruption::High,
            sound: None,
            vibration: None,
            light_color: None,
            show_badge: true,
            lock_screen: None,
            bypass_dnd: false,
            group: None,
        }]);
        assert!(
            s.prepare_show(n, 0)
                .unwrap()
                .notification
                .interruption
                .is_some()
        );
    }

    #[test]
    fn groups_and_all_ids_cover_both_tables() {
        let mut s = state();
        show(
            &mut s,
            Notification::new("a", "t")
                .unwrap()
                .with_group("g")
                .unwrap(),
            0,
        );
        s.schedule(
            Notification::new("b", "t")
                .unwrap()
                .with_group("g")
                .unwrap(),
            Trigger::After(60_000),
            0,
        )
        .unwrap();
        let mut ids = s.group_ids(&crate::GroupId::try_from("g").unwrap());
        ids.sort();
        assert_eq!(ids, [id("a"), id("b")]);
        assert_eq!(s.all_ids().len(), 2);
        assert_eq!(s.remove(&ids), vec![id("a")]);
        assert_eq!(s.pending(), Vec::new());
    }
}
