//! Constructors and `with_*` builders (Rust sugar, no wire impact).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::{
    Action, ActionId, ActionKind, CalendarRule, Capabilities, Category, ChannelId, ChatMessage,
    Chronometer, Conversation, Feature, Group, GroupId, Interruption, InvalidField, LockScreen,
    MissedPolicy, Notification, NotificationId, NotificationImage, NotificationPatch, Participant,
    PatchField, Progress, ReplyOptions, Sound, Style, Support, Trigger, Weekday,
};

/// Milliseconds of `duration`, saturating at `u64::MAX`.
pub(crate) fn duration_ms(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Unix-epoch milliseconds of `time` (negative before 1970), saturating.
pub(crate) fn system_time_ms(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_millis()).unwrap_or(i64::MAX),
        Err(before) => i64::try_from(before.duration().as_millis()).map_or(i64::MIN, |ms| -ms),
    }
}

/// The current Unix-epoch time in milliseconds.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn now_ms() -> i64 {
    system_time_ms(SystemTime::now())
}

/// The current Unix-epoch time in milliseconds: `Date.now()`
/// (`SystemTime::now` panics on `wasm32-unknown-unknown`).
#[cfg(target_arch = "wasm32")]
pub(crate) fn now_ms() -> i64 {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "Date.now() is an integral number of milliseconds well inside i64"
    )]
    let now = js_sys::Date::now() as i64;
    now
}

impl Notification {
    /// A plain notification with a validated id and a title; every other
    /// field is empty.
    pub fn new(id: &str, title: impl Into<String>) -> Result<Self, InvalidField> {
        Ok(Self::with_id(NotificationId::try_from(id)?, title))
    }

    /// [`Self::new`] with an id already built.
    #[must_use]
    pub fn with_id(id: NotificationId, title: impl Into<String>) -> Self {
        Self {
            id,
            channel: None,
            title: title.into(),
            subtitle: None,
            body: None,
            style: Style::Plain,
            icon: None,
            actions: Vec::new(),
            url: None,
            user_data: None,
            group: None,
            category: None,
            interruption: None,
            sound: None,
            progress: None,
            ongoing: false,
            live: false,
            status_text: None,
            timeout_ms: None,
            timestamp_ms: None,
            chronometer: None,
            count: None,
            lock_screen: None,
            hide_in_foreground: false,
            missed: MissedPolicy::default(),
            android: None,
            apple: None,
            linux_hints: Vec::new(),
        }
    }

    /// Sets the body.
    #[must_use]
    pub fn with_body(mut self, body: impl Into<String>) -> Self {
        self.body = Some(body.into());
        self
    }

    /// Sets the subtitle.
    #[must_use]
    pub fn with_subtitle(mut self, subtitle: impl Into<String>) -> Self {
        self.subtitle = Some(subtitle.into());
        self
    }

    /// Puts it in the channel `id` (validated).
    pub fn with_channel(mut self, id: &str) -> Result<Self, InvalidField> {
        self.channel = Some(ChannelId::try_from(id)?);
        Ok(self)
    }

    /// Sets the style.
    #[must_use]
    pub fn with_style(mut self, style: Style) -> Self {
        self.style = style;
        self
    }

    /// Sets the large icon / avatar.
    #[must_use]
    pub fn with_icon(mut self, icon: NotificationImage) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Appends an action.
    #[must_use]
    pub fn with_action(mut self, action: Action) -> Self {
        self.actions.push(action);
        self
    }

    /// Sets the URL opened on tap.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets the user data handed back in responses.
    #[must_use]
    pub fn with_user_data(mut self, data: impl Into<String>) -> Self {
        self.user_data = Some(data.into());
        self
    }

    /// Puts it in the group `id` (validated).
    pub fn with_group(mut self, id: &str) -> Result<Self, InvalidField> {
        self.group = Some(Group {
            id: GroupId::try_from(id)?,
            title: None,
            summary: None,
        });
        Ok(self)
    }

    /// Sets the category.
    #[must_use]
    pub const fn with_category(mut self, category: Category) -> Self {
        self.category = Some(category);
        self
    }

    /// Sets the interruption level.
    #[must_use]
    pub const fn with_interruption(mut self, level: Interruption) -> Self {
        self.interruption = Some(level);
        self
    }

    /// Sets the sound.
    #[must_use]
    pub fn with_sound(mut self, sound: Sound) -> Self {
        self.sound = Some(sound);
        self
    }

    /// Sets the progress bar.
    #[must_use]
    pub fn with_progress(mut self, progress: Progress) -> Self {
        self.progress = Some(progress);
        self
    }

    /// Sets the short status text.
    #[must_use]
    pub fn with_status_text(mut self, text: impl Into<String>) -> Self {
        self.status_text = Some(text.into());
        self
    }

    /// Removes it after `timeout`.
    #[must_use]
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout_ms = Some(duration_ms(timeout));
        self
    }

    /// Stamps it with `time`.
    #[must_use]
    pub fn with_timestamp(mut self, time: SystemTime) -> Self {
        self.timestamp_ms = Some(system_time_ms(time));
        self
    }

    /// Shows a running clock.
    #[must_use]
    pub const fn with_chronometer(mut self, chronometer: Chronometer) -> Self {
        self.chronometer = Some(chronometer);
        self
    }

    /// Sets the number shown on it.
    #[must_use]
    pub const fn with_count(mut self, count: u32) -> Self {
        self.count = Some(count);
        self
    }

    /// Sets the lock-screen visibility.
    #[must_use]
    pub const fn with_lock_screen(mut self, lock_screen: LockScreen) -> Self {
        self.lock_screen = Some(lock_screen);
        self
    }

    /// Sets the missed-delivery policy (scheduled notifications).
    #[must_use]
    pub const fn with_missed(mut self, missed: MissedPolicy) -> Self {
        self.missed = missed;
        self
    }

    /// Makes it ongoing (not dismissable).
    #[must_use]
    pub const fn ongoing(mut self) -> Self {
        self.ongoing = true;
        self
    }

    /// Asks Android 16 QPR1+ to promote it as a Live Update (implies
    /// ongoing).
    #[must_use]
    pub const fn live(mut self) -> Self {
        self.live = true;
        self.ongoing = true;
        self
    }

    /// Hides it while the app is in the foreground.
    #[must_use]
    pub const fn hidden_in_foreground(mut self) -> Self {
        self.hide_in_foreground = true;
        self
    }

    /// Applies `patch`: every `Some` replaces, then `clear` removes.
    pub fn apply(&mut self, patch: NotificationPatch) {
        let NotificationPatch {
            title,
            subtitle,
            body,
            style,
            icon,
            actions,
            progress,
            status_text,
            count,
            ongoing,
            url,
            user_data,
            clear,
            alert_again: _,
        } = patch;
        if let Some(title) = title {
            self.title = title;
        }
        replace(&mut self.subtitle, subtitle);
        replace(&mut self.body, body);
        if let Some(style) = style {
            self.style = style;
        }
        replace(&mut self.icon, icon);
        if let Some(actions) = actions {
            self.actions = actions;
        }
        replace(&mut self.progress, progress);
        replace(&mut self.status_text, status_text);
        replace(&mut self.count, count);
        if let Some(ongoing) = ongoing {
            self.ongoing = ongoing;
        }
        replace(&mut self.url, url);
        replace(&mut self.user_data, user_data);
        for field in clear {
            match field {
                PatchField::Subtitle => self.subtitle = None,
                PatchField::Body => self.body = None,
                PatchField::Icon => self.icon = None,
                PatchField::Progress => self.progress = None,
                PatchField::StatusText => self.status_text = None,
                PatchField::Count => self.count = None,
                PatchField::Url => self.url = None,
                PatchField::UserData => self.user_data = None,
                PatchField::Actions => self.actions.clear(),
            }
        }
    }
}

fn replace<T>(slot: &mut Option<T>, value: Option<T>) {
    if value.is_some() {
        *slot = value;
    }
}

impl NotificationPatch {
    /// A patch that only sets (or, with `None`, removes) the progress.
    #[must_use]
    pub fn progress(progress: Option<Progress>) -> Self {
        progress.map_or_else(
            || Self {
                clear: vec![PatchField::Progress],
                ..Self::default()
            },
            |progress| Self {
                progress: Some(progress),
                ..Self::default()
            },
        )
    }

    /// Alerts again instead of updating silently.
    #[must_use]
    pub const fn alerting(mut self) -> Self {
        self.alert_again = true;
        self
    }
}

impl Progress {
    /// A determinate bar at `value` (clamped to `0.0..=1.0`; NaN is 0).
    #[must_use]
    pub const fn fraction(value: f32) -> Self {
        let value = if value.is_nan() {
            0.0
        } else {
            value.clamp(0.0, 1.0)
        };
        Self {
            value: Some(value),
            label: None,
            segments: Vec::new(),
            points: Vec::new(),
        }
    }

    /// An indeterminate bar.
    #[must_use]
    pub const fn indeterminate() -> Self {
        Self {
            value: None,
            label: None,
            segments: Vec::new(),
            points: Vec::new(),
        }
    }

    /// Sets the label.
    #[must_use]
    pub fn with_label(mut self, label: impl Into<String>) -> Self {
        self.label = Some(label.into());
        self
    }
}

impl Action {
    fn of(id: &str, title: impl Into<String>, kind: ActionKind) -> Result<Self, InvalidField> {
        Ok(Self {
            id: ActionId::try_from(id)?,
            title: title.into(),
            kind,
            opens_app: true,
            destructive: false,
            requires_unlock: false,
            url: None,
            icon: None,
        })
    }

    /// A button that opens the app.
    pub fn button(id: &str, title: impl Into<String>) -> Result<Self, InvalidField> {
        Self::of(id, title, ActionKind::Button)
    }

    /// An inline reply field.
    pub fn reply(id: &str, title: impl Into<String>) -> Result<Self, InvalidField> {
        Self::of(id, title, ActionKind::Reply(ReplyOptions::default()))
    }

    /// A fixed set of choices; the chosen one comes back as the reply.
    pub fn select<I, S>(
        id: &str,
        title: impl Into<String>,
        choices: I,
    ) -> Result<Self, InvalidField>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let choices = choices.into_iter().map(Into::into).collect();
        Self::of(id, title, ActionKind::Select(choices))
    }

    /// Hides the notification and shows it again after `delay`.
    pub fn snooze(
        id: &str,
        title: impl Into<String>,
        delay: Duration,
    ) -> Result<Self, InvalidField> {
        let mut action = Self::of(id, title, ActionKind::Snooze(duration_ms(delay)))?;
        action.opens_app = false;
        Ok(action)
    }

    /// Handled in the background: the app is not brought forward and the
    /// response carries an ack token.
    #[must_use]
    pub const fn background(mut self) -> Self {
        self.opens_app = false;
        self
    }

    /// Shown as destructive.
    #[must_use]
    pub const fn destructive(mut self) -> Self {
        self.destructive = true;
        self
    }

    /// Needs the device unlocked.
    #[must_use]
    pub const fn requires_unlock(mut self) -> Self {
        self.requires_unlock = true;
        self
    }

    /// Publishes `url` as a deep link with the response.
    #[must_use]
    pub fn with_url(mut self, url: impl Into<String>) -> Self {
        self.url = Some(url.into());
        self
    }

    /// Sets the button icon.
    #[must_use]
    pub fn with_icon(mut self, icon: NotificationImage) -> Self {
        self.icon = Some(icon);
        self
    }

    /// Reply options (placeholder, submit label, suggestions); ignored
    /// unless the action is a reply.
    #[must_use]
    pub fn with_reply_options(mut self, options: ReplyOptions) -> Self {
        if let ActionKind::Reply(current) = &mut self.kind {
            *current = options;
        }
        self
    }
}

impl NotificationImage {
    /// PNG bytes.
    #[must_use]
    pub const fn png(bytes: Vec<u8>) -> Self {
        Self::Png(bytes)
    }

    /// JPEG bytes.
    #[must_use]
    pub const fn jpeg(bytes: Vec<u8>) -> Self {
        Self::Jpeg(bytes)
    }

    /// An image file (absolute path).
    #[must_use]
    pub fn file(path: impl Into<String>) -> Self {
        Self::File(path.into())
    }

    /// A platform image name.
    #[must_use]
    pub fn named(name: impl Into<String>) -> Self {
        Self::Named(name.into())
    }
}

impl Participant {
    /// Someone with a stable `key` and a display `name`.
    #[must_use]
    pub fn new(key: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            name: name.into(),
            avatar: None,
            is_bot: false,
            important: false,
        }
    }

    /// Sets the avatar.
    #[must_use]
    pub fn with_avatar(mut self, avatar: NotificationImage) -> Self {
        self.avatar = Some(avatar);
        self
    }

    /// Marks an automated sender.
    #[must_use]
    pub const fn bot(mut self) -> Self {
        self.is_bot = true;
        self
    }

    /// Marks an important contact.
    #[must_use]
    pub const fn important(mut self) -> Self {
        self.important = true;
        self
    }
}

impl ChatMessage {
    /// A message from `sender`, stamped now.
    #[must_use]
    pub fn received(sender: Participant, text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            timestamp_ms: now_ms(),
            sender: Some(sender),
            image: None,
        }
    }

    /// A message the user sent, stamped now.
    #[must_use]
    pub fn sent(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            timestamp_ms: now_ms(),
            sender: None,
            image: None,
        }
    }

    /// Sets the time it was sent.
    #[must_use]
    pub fn at(mut self, time: SystemTime) -> Self {
        self.timestamp_ms = system_time_ms(time);
        self
    }

    /// Attaches a picture.
    #[must_use]
    pub fn with_image(mut self, image: NotificationImage) -> Self {
        self.image = Some(image);
        self
    }
}

impl Conversation {
    /// A one-to-one conversation of `user` (the device owner).
    #[must_use]
    pub const fn new(user: Participant) -> Self {
        Self {
            user,
            title: None,
            is_group: false,
            messages: Vec::new(),
        }
    }

    /// A group conversation called `title`.
    #[must_use]
    pub fn group(user: Participant, title: impl Into<String>) -> Self {
        Self {
            user,
            title: Some(title.into()),
            is_group: true,
            messages: Vec::new(),
        }
    }

    /// Appends a message.
    #[must_use]
    pub fn with_message(mut self, message: ChatMessage) -> Self {
        self.messages.push(message);
        self
    }
}

impl Trigger {
    /// Once, `delay` from now.
    #[must_use]
    pub fn after(delay: Duration) -> Self {
        Self::After(duration_ms(delay))
    }

    /// Once, at `time`.
    #[must_use]
    pub fn at(time: SystemTime) -> Self {
        Self::At(system_time_ms(time))
    }

    /// Every `period` (at least a minute).
    #[must_use]
    pub fn every(period: Duration) -> Self {
        Self::Every(duration_ms(period))
    }

    /// Every day at `hour:minute`, local wall clock.
    #[must_use]
    pub const fn daily(hour: u8, minute: u8) -> Self {
        Self::Calendar(CalendarRule::daily(hour, minute))
    }

    /// On `weekdays` at `hour:minute`, local wall clock.
    #[must_use]
    pub fn weekly(weekdays: &[Weekday], hour: u8, minute: u8) -> Self {
        Self::Calendar(CalendarRule::daily(hour, minute).on(weekdays))
    }
}

impl CalendarRule {
    /// Every day at `hour:minute`, local wall clock, forever.
    #[must_use]
    pub const fn daily(hour: u8, minute: u8) -> Self {
        Self {
            hour,
            minute,
            weekdays: Vec::new(),
            day_of_month: None,
            month: None,
            time_zone: None,
            until_ms: None,
        }
    }

    /// Only on `weekdays`.
    #[must_use]
    pub fn on(mut self, weekdays: &[Weekday]) -> Self {
        self.weekdays = weekdays.to_vec();
        self
    }

    /// Only on day `day` of the month.
    #[must_use]
    pub const fn on_day(mut self, day: u8) -> Self {
        self.day_of_month = Some(day);
        self
    }

    /// Only in month `month` (`1..=12`).
    #[must_use]
    pub const fn in_month(mut self, month: u8) -> Self {
        self.month = Some(month);
        self
    }

    /// In the IANA zone `zone` instead of the local wall clock.
    #[must_use]
    pub fn in_zone(mut self, zone: impl Into<String>) -> Self {
        self.time_zone = Some(zone.into());
        self
    }

    /// No occurrence after `time`.
    #[must_use]
    pub fn until(mut self, time: SystemTime) -> Self {
        self.until_ms = Some(system_time_ms(time));
        self
    }
}

impl Capabilities {
    /// How `feature` is supported ([`Support::Unsupported`] when not
    /// listed).
    #[must_use]
    pub fn support(&self, feature: Feature) -> Support {
        self.features
            .iter()
            .find(|entry| entry.feature == feature)
            .map_or(Support::Unsupported, |entry| entry.support)
    }

    /// Whether `feature` works at all (natively or simulated).
    #[must_use]
    pub fn has(&self, feature: Feature) -> bool {
        self.support(feature) != Support::Unsupported
    }
}
