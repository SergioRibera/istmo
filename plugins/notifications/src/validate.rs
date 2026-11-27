//! Validation: the limits every host enforces before a platform sees a
//! notification.
//!
//! Rust clients run it before the call ([`crate::NotificationsClient::show_checked`]),
//! the Rust desktop host runs it again, and native backends re-check the
//! limits of their platform. Errors name the field by path
//! (`actions[2].title`, `trigger.hour`) in [`InvalidField::field`].
//!
//! | Field | Limit |
//! |---|---|
//! | ids | `1..=64` bytes of `[A-Za-z0-9._:-]` ([`crate::MAX_ID_LEN`]) |
//! | `title` | not empty, [`MAX_TITLE_CHARS`] characters, no control characters |
//! | `subtitle`, group texts | [`MAX_TITLE_CHARS`] |
//! | `body`, big text | [`MAX_BODY_CHARS`] |
//! | `status_text` | [`MAX_STATUS_CHARS`] |
//! | `actions` | [`MAX_ACTIONS`]; unique ids; `istmo.` ids only the call ones under [`Style::Call`] |
//! | action titles, choices, suggestions | not empty, [`MAX_LABEL_CHARS`] |
//! | choices, suggestions | [`MAX_CHOICES`] each |
//! | snooze | from [`MIN_SNOOZE_MS`] to [`MAX_SNOOZE_MS`] |
//! | inbox lines | [`MAX_INBOX_LINES`] |
//! | `url` | [`MAX_URL_LEN`] bytes, an RFC 3986 scheme, no whitespace or control characters |
//! | `user_data` | [`MAX_USER_DATA_LEN`] bytes |
//! | image bytes | [`MAX_IMAGE_BYTES`]; files absolute; names [`MAX_LABEL_CHARS`] |
//! | progress | value finite in `0.0..=1.0`; [`MAX_PROGRESS_MARKS`] segments and points |
//! | `timeout_ms` | greater than 0 |
//! | Apple `relevance_score` | `0.0..=1.0` |
//! | Linux hints | key `1..=64` bytes, bytes [`MAX_IMAGE_BYTES`] |
//! | `Trigger::Every` | at least [`MIN_REPEAT_MS`] |
//! | calendar rule | hour `0..=23`, minute `0..=59`, day `1..=31` possible in `month`, month `1..=12`, unique weekdays, a known IANA zone |
//! | channel | name not empty, [`MAX_LABEL_CHARS`] * 2; description [`MAX_BODY_CHARS`]; vibration [`MAX_VIBRATION_STEPS`] |
//!
//! Conversations keep their last [`MAX_MESSAGES`] messages
//! ([`Notification::normalize`]); more is not an error.

use std::collections::HashSet;

use crate::ids::check_id;
use crate::{
    Action, ActionId, ActionKind, CalendarRule, ChannelSpec, ChatMessage, HintValue, InvalidField,
    LinuxHint, Notification, NotificationImage, NotificationPatch, Participant, PatchField,
    Problem, Progress, Style, Trigger,
};

/// Longest title / subtitle, in characters.
pub const MAX_TITLE_CHARS: usize = 256;
/// Longest body, in characters.
pub const MAX_BODY_CHARS: usize = 4096;
/// Longest status text, in characters.
pub const MAX_STATUS_CHARS: usize = 64;
/// Longest label (action title, choice, participant name), in characters.
pub const MAX_LABEL_CHARS: usize = 128;
/// Most actions on one notification (platforms show fewer; the rest
/// become a "More" action).
pub const MAX_ACTIONS: usize = 16;
/// Most choices of a select action, and suggestions of a reply.
pub const MAX_CHOICES: usize = 10;
/// Most inbox lines.
pub const MAX_INBOX_LINES: usize = 16;
/// Messages a conversation keeps.
pub const MAX_MESSAGES: usize = 25;
/// Longest URL, in bytes.
pub const MAX_URL_LEN: usize = 2048;
/// Largest user data, in bytes.
pub const MAX_USER_DATA_LEN: usize = 3 * 1024;
/// Largest image (or hint) payload, in bytes.
pub const MAX_IMAGE_BYTES: usize = 2 * 1024 * 1024;
/// Most progress segments, and points.
pub const MAX_PROGRESS_MARKS: usize = 32;
/// Most vibration pattern steps.
pub const MAX_VIBRATION_STEPS: usize = 32;
/// Shortest interval of [`Trigger::Every`].
pub const MIN_REPEAT_MS: u64 = 60_000;
/// Shortest snooze.
pub const MIN_SNOOZE_MS: u64 = 1_000;
/// Longest snooze: 30 days.
pub const MAX_SNOOZE_MS: u64 = 30 * 24 * 3_600_000;

type Checked = Result<(), Problem>;

/// `value` is at most `max` characters, without control characters
/// (newlines and tabs pass when `multiline`).
fn text(value: &str, max: usize, multiline: bool) -> Checked {
    let mut count = 0usize;
    for c in value.chars() {
        count += 1;
        if count > max {
            return Err(Problem::TooLong);
        }
        if c.is_control() && !(multiline && matches!(c, '\n' | '\t')) {
            return Err(Problem::BadCharset);
        }
    }
    Ok(())
}

/// [`text`], and not empty (or only whitespace).
fn label(value: &str, max: usize) -> Checked {
    if value.trim().is_empty() {
        return Err(Problem::Empty);
    }
    text(value, max, false)
}

fn optional(value: Option<&str>, max: usize, multiline: bool) -> Checked {
    value.map_or(Ok(()), |value| text(value, max, multiline))
}

/// An RFC 3986 scheme, then no whitespace or control characters.
fn url(value: &str) -> Checked {
    if value.len() > MAX_URL_LEN {
        return Err(Problem::TooLong);
    }
    let Some((scheme, _)) = value.split_once(':') else {
        return Err(Problem::BadUrl);
    };
    let mut bytes = scheme.bytes();
    let starts = bytes.next().is_some_and(|b| b.is_ascii_alphabetic());
    let rest = bytes.all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'));
    if !starts || !rest || value.chars().any(|c| c.is_whitespace() || c.is_control()) {
        return Err(Problem::BadUrl);
    }
    Ok(())
}

fn image(value: &NotificationImage) -> Checked {
    match value {
        NotificationImage::Png(bytes) | NotificationImage::Jpeg(bytes) => {
            if bytes.is_empty() {
                Err(Problem::Empty)
            } else if bytes.len() > MAX_IMAGE_BYTES {
                Err(Problem::TooLarge)
            } else {
                Ok(())
            }
        }
        NotificationImage::File(path) => {
            if path.is_empty() {
                Err(Problem::Empty)
            } else if !std::path::Path::new(path).is_absolute() && !path.starts_with('/') {
                Err(Problem::NotAllowedHere)
            } else {
                text(path, MAX_URL_LEN, false)
            }
        }
        NotificationImage::Named(name) => label(name, MAX_LABEL_CHARS),
    }
}

/// Runs `check`; on failure names the field `path()`.
fn at(check: Checked, path: impl FnOnce() -> String) -> Result<(), InvalidField> {
    check.map_err(|problem| InvalidField::new(path(), problem))
}

fn participant(value: &Participant, path: &str) -> Result<(), InvalidField> {
    at(label(&value.key, MAX_LABEL_CHARS), || format!("{path}.key"))?;
    at(label(&value.name, MAX_LABEL_CHARS), || {
        format!("{path}.name")
    })?;
    if let Some(avatar) = &value.avatar {
        at(image(avatar), || format!("{path}.avatar"))?;
    }
    Ok(())
}

fn message(value: &ChatMessage, path: &str) -> Result<(), InvalidField> {
    at(label(&value.text, MAX_BODY_CHARS), || {
        format!("{path}.text")
    })?;
    if let Some(sender) = &value.sender {
        participant(sender, &format!("{path}.sender"))?;
    }
    if let Some(picture) = &value.image {
        at(image(picture), || format!("{path}.image"))?;
    }
    Ok(())
}

fn style(value: &Style, path: &str) -> Result<(), InvalidField> {
    match value {
        Style::Plain => Ok(()),
        Style::BigText(body) => at(text(body, MAX_BODY_CHARS, true), || path.to_owned()),
        Style::Inbox(lines) => {
            if lines.len() > MAX_INBOX_LINES {
                return Err(InvalidField::new(path, Problem::TooLong));
            }
            for (i, line) in lines.iter().enumerate() {
                at(text(line, MAX_TITLE_CHARS, false), || {
                    format!("{path}[{i}]")
                })?;
            }
            Ok(())
        }
        Style::Picture(picture) => at(image(picture), || path.to_owned()),
        Style::Messaging(conversation) => {
            participant(&conversation.user, &format!("{path}.user"))?;
            at(
                optional(conversation.title.as_deref(), MAX_TITLE_CHARS, false),
                || format!("{path}.title"),
            )?;
            for (i, msg) in conversation.messages.iter().enumerate() {
                message(msg, &format!("{path}.messages[{i}]"))?;
            }
            Ok(())
        }
        Style::Call(call) => participant(&call.caller, &format!("{path}.caller")),
    }
}

fn progress(value: &Progress, path: &str) -> Result<(), InvalidField> {
    if let Some(fraction) = value.value
        && !(fraction.is_finite() && (0.0..=1.0).contains(&fraction))
    {
        return Err(InvalidField::new(
            format!("{path}.value"),
            Problem::OutOfRange,
        ));
    }
    at(
        optional(value.label.as_deref(), MAX_LABEL_CHARS, false),
        || format!("{path}.label"),
    )?;
    if value.segments.len() > MAX_PROGRESS_MARKS {
        return Err(InvalidField::new(
            format!("{path}.segments"),
            Problem::TooLong,
        ));
    }
    if value.points.len() > MAX_PROGRESS_MARKS {
        return Err(InvalidField::new(
            format!("{path}.points"),
            Problem::TooLong,
        ));
    }
    Ok(())
}

fn choices(values: &[String], path: &str) -> Result<(), InvalidField> {
    if values.len() > MAX_CHOICES {
        return Err(InvalidField::new(path, Problem::TooLong));
    }
    let mut seen = HashSet::with_capacity(values.len());
    for (i, choice) in values.iter().enumerate() {
        at(label(choice, MAX_LABEL_CHARS), || format!("{path}[{i}]"))?;
        if !seen.insert(choice.as_str()) {
            return Err(InvalidField::new(
                format!("{path}[{i}]"),
                Problem::Duplicate,
            ));
        }
    }
    Ok(())
}

/// The reserved ids a [`Style::Call`] notification may carry.
const CALL_ACTIONS: [&str; 3] = [
    ActionId::CALL_ANSWER,
    ActionId::CALL_DECLINE,
    ActionId::CALL_HANG_UP,
];

fn actions(values: &[Action], is_call: bool) -> Result<(), InvalidField> {
    if values.len() > MAX_ACTIONS {
        return Err(InvalidField::new("actions", Problem::TooLong));
    }
    let mut seen = HashSet::with_capacity(values.len());
    for (i, action) in values.iter().enumerate() {
        at(check_id_problem(action.id.as_str()), || {
            format!("actions[{i}].id")
        })?;
        if action.id.is_reserved() && !(is_call && CALL_ACTIONS.contains(&action.id.as_str())) {
            return Err(InvalidField::new(
                format!("actions[{i}].id"),
                Problem::Reserved,
            ));
        }
        if !seen.insert(action.id.as_str()) {
            return Err(InvalidField::new(
                format!("actions[{i}].id"),
                Problem::Duplicate,
            ));
        }
        at(label(&action.title, MAX_LABEL_CHARS), || {
            format!("actions[{i}].title")
        })?;
        if let Some(link) = &action.url {
            at(url(link), || format!("actions[{i}].url"))?;
        }
        if let Some(icon) = &action.icon {
            at(image(icon), || format!("actions[{i}].icon"))?;
        }
        match &action.kind {
            ActionKind::Button => {}
            ActionKind::Reply(options) => {
                at(
                    optional(options.placeholder.as_deref(), MAX_LABEL_CHARS, false),
                    || format!("actions[{i}].placeholder"),
                )?;
                at(
                    optional(options.submit_title.as_deref(), MAX_LABEL_CHARS, false),
                    || format!("actions[{i}].submit_title"),
                )?;
                choices(&options.suggestions, &format!("actions[{i}].suggestions"))?;
            }
            ActionKind::Select(list) => {
                if list.is_empty() {
                    return Err(InvalidField::new(
                        format!("actions[{i}].choices"),
                        Problem::Empty,
                    ));
                }
                choices(list, &format!("actions[{i}].choices"))?;
            }
            ActionKind::Snooze(ms) => {
                if !(MIN_SNOOZE_MS..=MAX_SNOOZE_MS).contains(ms) {
                    return Err(InvalidField::new(
                        format!("actions[{i}].snooze"),
                        Problem::OutOfRange,
                    ));
                }
            }
        }
    }
    Ok(())
}

fn check_id_problem(value: &str) -> Checked {
    check_id("", value).map_err(|err| err.problem)
}

fn hints(values: &[LinuxHint]) -> Result<(), InvalidField> {
    for (i, hint) in values.iter().enumerate() {
        if hint.key.is_empty() {
            return Err(InvalidField::new(
                format!("linux_hints[{i}].key"),
                Problem::Empty,
            ));
        }
        if hint.key.len() > 64 || !hint.key.is_ascii() {
            return Err(InvalidField::new(
                format!("linux_hints[{i}].key"),
                Problem::BadCharset,
            ));
        }
        let size = match &hint.value {
            HintValue::Bool(_) | HintValue::Number(_) => 0,
            HintValue::Text(text) => text.len(),
            HintValue::Bytes(bytes) => bytes.len(),
        };
        if size > MAX_IMAGE_BYTES {
            return Err(InvalidField::new(
                format!("linux_hints[{i}].value"),
                Problem::TooLarge,
            ));
        }
    }
    Ok(())
}

impl Notification {
    /// Checks every field against the [limits](self).
    pub fn validate(&self) -> Result<(), InvalidField> {
        self.id.validate()?;
        if let Some(channel) = &self.channel {
            channel.validate()?;
        }
        at(label(&self.title, MAX_TITLE_CHARS), || "title".to_owned())?;
        at(
            optional(self.subtitle.as_deref(), MAX_TITLE_CHARS, false),
            || "subtitle".to_owned(),
        )?;
        at(optional(self.body.as_deref(), MAX_BODY_CHARS, true), || {
            "body".to_owned()
        })?;
        style(&self.style, "style")?;
        if let Some(icon) = &self.icon {
            at(image(icon), || "icon".to_owned())?;
        }
        actions(&self.actions, matches!(self.style, Style::Call(_)))?;
        if let Some(link) = &self.url {
            at(url(link), || "url".to_owned())?;
        }
        if let Some(data) = &self.user_data
            && data.len() > MAX_USER_DATA_LEN
        {
            return Err(InvalidField::new("user_data", Problem::TooLarge));
        }
        if let Some(group) = &self.group {
            group.id.validate()?;
            at(
                optional(group.title.as_deref(), MAX_TITLE_CHARS, false),
                || "group.title".to_owned(),
            )?;
            at(
                optional(group.summary.as_deref(), MAX_TITLE_CHARS, false),
                || "group.summary".to_owned(),
            )?;
        }
        if let Some(bar) = &self.progress {
            progress(bar, "progress")?;
        }
        at(
            optional(self.status_text.as_deref(), MAX_STATUS_CHARS, false),
            || "status_text".to_owned(),
        )?;
        if self.timeout_ms == Some(0) {
            return Err(InvalidField::new("timeout_ms", Problem::OutOfRange));
        }
        if let Some(apple) = &self.apple
            && let Some(score) = apple.relevance_score
            && !(score.is_finite() && (0.0..=1.0).contains(&score))
        {
            return Err(InvalidField::new(
                "apple.relevance_score",
                Problem::OutOfRange,
            ));
        }
        hints(&self.linux_hints)
    }

    /// Keeps the last [`MAX_MESSAGES`] messages of a conversation.
    pub fn normalize(&mut self) {
        if let Style::Messaging(conversation) = &mut self.style {
            let extra = conversation.messages.len().saturating_sub(MAX_MESSAGES);
            conversation.messages.drain(..extra);
        }
    }
}

impl NotificationPatch {
    /// Checks the fields it sets against the [limits](self), and that it
    /// does not set a field it also clears.
    pub fn validate(&self) -> Result<(), InvalidField> {
        if let Some(title) = &self.title {
            at(label(title, MAX_TITLE_CHARS), || "title".to_owned())?;
        }
        at(
            optional(self.subtitle.as_deref(), MAX_TITLE_CHARS, false),
            || "subtitle".to_owned(),
        )?;
        at(optional(self.body.as_deref(), MAX_BODY_CHARS, true), || {
            "body".to_owned()
        })?;
        if let Some(value) = &self.style {
            style(value, "style")?;
        }
        if let Some(icon) = &self.icon {
            at(image(icon), || "icon".to_owned())?;
        }
        if let Some(list) = &self.actions {
            actions(list, matches!(self.style, Some(Style::Call(_))))?;
        }
        if let Some(bar) = &self.progress {
            progress(bar, "progress")?;
        }
        at(
            optional(self.status_text.as_deref(), MAX_STATUS_CHARS, false),
            || "status_text".to_owned(),
        )?;
        if let Some(link) = &self.url {
            at(url(link), || "url".to_owned())?;
        }
        if let Some(data) = &self.user_data
            && data.len() > MAX_USER_DATA_LEN
        {
            return Err(InvalidField::new("user_data", Problem::TooLarge));
        }
        for field in &self.clear {
            let (set, name) = match field {
                PatchField::Subtitle => (self.subtitle.is_some(), "subtitle"),
                PatchField::Body => (self.body.is_some(), "body"),
                PatchField::Icon => (self.icon.is_some(), "icon"),
                PatchField::Progress => (self.progress.is_some(), "progress"),
                PatchField::StatusText => (self.status_text.is_some(), "status_text"),
                PatchField::Count => (self.count.is_some(), "count"),
                PatchField::Url => (self.url.is_some(), "url"),
                PatchField::UserData => (self.user_data.is_some(), "user_data"),
                PatchField::Actions => (self.actions.is_some(), "actions"),
            };
            if set {
                return Err(InvalidField::new(name, Problem::NotAllowedHere));
            }
        }
        Ok(())
    }
}

impl ChatMessage {
    /// Checks the message against the [limits](self).
    pub fn validate(&self) -> Result<(), InvalidField> {
        message(self, "message")
    }
}

impl Trigger {
    /// Checks the trigger against the [limits](self).
    pub fn validate(&self) -> Result<(), InvalidField> {
        match self {
            Self::After(_) | Self::At(_) => Ok(()),
            Self::Every(ms) => {
                if *ms < MIN_REPEAT_MS {
                    Err(InvalidField::new("trigger.every", Problem::OutOfRange))
                } else {
                    Ok(())
                }
            }
            Self::Calendar(rule) => rule.validate(),
        }
    }
}

/// Longest day of `month` (`1..=12`), leap years included.
const fn max_day(month: u8) -> u8 {
    match month {
        2 => 29,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

impl CalendarRule {
    /// Checks the rule: ranges, unique weekdays, a day that exists in
    /// `month`, a known IANA zone.
    pub fn validate(&self) -> Result<(), InvalidField> {
        if self.hour > 23 {
            return Err(InvalidField::new("trigger.hour", Problem::OutOfRange));
        }
        if self.minute > 59 {
            return Err(InvalidField::new("trigger.minute", Problem::OutOfRange));
        }
        if let Some(month) = self.month
            && !(1..=12).contains(&month)
        {
            return Err(InvalidField::new("trigger.month", Problem::OutOfRange));
        }
        if let Some(day) = self.day_of_month {
            let max = self.month.map_or(31, max_day);
            if !(1..=max).contains(&day) {
                return Err(InvalidField::new(
                    "trigger.day_of_month",
                    Problem::OutOfRange,
                ));
            }
        }
        let mut seen = 0u8;
        for day in &self.weekdays {
            let bit = 1u8 << (*day as u8);
            if seen & bit != 0 {
                return Err(InvalidField::new("trigger.weekdays", Problem::Duplicate));
            }
            seen |= bit;
        }
        if let Some(zone) = &self.time_zone
            && jiff::tz::TimeZone::get(zone).is_err()
        {
            return Err(InvalidField::new("trigger.time_zone", Problem::OutOfRange));
        }
        Ok(())
    }
}

impl ChannelSpec {
    /// Checks the channel against the [limits](self).
    pub fn validate(&self) -> Result<(), InvalidField> {
        self.id.validate()?;
        at(label(&self.name, MAX_LABEL_CHARS * 2), || "name".to_owned())?;
        at(
            optional(self.description.as_deref(), MAX_BODY_CHARS, true),
            || "description".to_owned(),
        )?;
        if self
            .vibration
            .as_ref()
            .is_some_and(|steps| steps.len() > MAX_VIBRATION_STEPS)
        {
            return Err(InvalidField::new("vibration", Problem::TooLong));
        }
        at(
            optional(self.group.as_deref(), MAX_LABEL_CHARS, false),
            || "group".to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CallInfo, CallKind, Conversation, Weekday};

    fn n() -> Notification {
        Notification::new("a", "Title").unwrap()
    }

    fn problem(result: Result<(), InvalidField>) -> (String, Problem) {
        let err = result.unwrap_err();
        (err.field, err.problem)
    }

    #[test]
    fn titles_must_be_present_and_plain() {
        assert!(n().validate().is_ok());
        let mut blank = n();
        blank.title = "  ".to_owned();
        assert_eq!(
            problem(blank.validate()),
            ("title".to_owned(), Problem::Empty)
        );
        let mut control = n();
        control.title = "a\nb".to_owned();
        assert_eq!(
            problem(control.validate()),
            ("title".to_owned(), Problem::BadCharset)
        );
        let multiline = n().with_body("line 1\nline 2");
        assert!(multiline.validate().is_ok());
        let long = n().with_body("x".repeat(MAX_BODY_CHARS + 1));
        assert_eq!(
            problem(long.validate()),
            ("body".to_owned(), Problem::TooLong)
        );
    }

    #[test]
    fn actions_are_unique_and_not_reserved() {
        let twice = n()
            .with_action(Action::button("a", "A").unwrap())
            .with_action(Action::button("a", "B").unwrap());
        assert_eq!(
            problem(twice.validate()),
            ("actions[1].id".to_owned(), Problem::Duplicate)
        );
        let mut reserved = Action::button("x", "X").unwrap();
        reserved.id = ActionId::reserved(ActionId::CALL_ANSWER);
        let plain = n().with_action(reserved.clone());
        assert_eq!(
            problem(plain.validate()),
            ("actions[0].id".to_owned(), Problem::Reserved)
        );
        let call = n()
            .with_style(Style::Call(CallInfo {
                caller: Participant::new("ana", "Ana"),
                kind: CallKind::Incoming,
                video: false,
            }))
            .with_action(reserved);
        assert!(call.validate().is_ok());
        let empty = n().with_action(Action::select("s", "Pick", Vec::<String>::new()).unwrap());
        assert_eq!(
            problem(empty.validate()),
            ("actions[0].choices".to_owned(), Problem::Empty)
        );
        let snooze = n().with_action(
            Action::snooze("z", "Later", std::time::Duration::from_millis(10)).unwrap(),
        );
        assert_eq!(
            problem(snooze.validate()),
            ("actions[0].snooze".to_owned(), Problem::OutOfRange)
        );
    }

    #[test]
    fn urls_need_a_scheme() {
        assert!(n().with_url("app://files/42").validate().is_ok());
        assert!(n().with_url("mailto:a@b.c").validate().is_ok());
        for bad in ["files/42", "1app://x", "app://a b", ":x"] {
            assert_eq!(
                problem(n().with_url(bad).validate()),
                ("url".to_owned(), Problem::BadUrl),
                "{bad}"
            );
        }
    }

    #[test]
    fn payload_sizes_are_capped() {
        let big = n().with_icon(NotificationImage::png(vec![0; MAX_IMAGE_BYTES + 1]));
        assert_eq!(
            problem(big.validate()),
            ("icon".to_owned(), Problem::TooLarge)
        );
        let data = n().with_user_data("x".repeat(MAX_USER_DATA_LEN + 1));
        assert_eq!(
            problem(data.validate()),
            ("user_data".to_owned(), Problem::TooLarge)
        );
        let relative = n().with_icon(NotificationImage::file("icon.png"));
        assert_eq!(
            problem(relative.validate()),
            ("icon".to_owned(), Problem::NotAllowedHere)
        );
    }

    #[test]
    fn progress_values_are_fractions() {
        let mut bar = Progress::fraction(0.5);
        bar.value = Some(1.5);
        assert_eq!(
            problem(n().with_progress(bar).validate()),
            ("progress.value".to_owned(), Problem::OutOfRange)
        );
        let mut nan = Progress::indeterminate();
        nan.value = Some(f32::NAN);
        assert!(n().with_progress(nan).validate().is_err());
        assert!(
            n().with_progress(Progress::indeterminate())
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn conversations_keep_the_last_messages() {
        let mut conversation = Conversation::new(Participant::new("me", "Me"));
        for i in 0..30 {
            conversation
                .messages
                .push(ChatMessage::sent(format!("m{i}")));
        }
        let mut chat = n().with_style(Style::Messaging(conversation));
        assert!(chat.validate().is_ok());
        chat.normalize();
        let Style::Messaging(conversation) = &chat.style else {
            unreachable!()
        };
        assert_eq!(conversation.messages.len(), MAX_MESSAGES);
        assert_eq!(conversation.messages[0].text, "m5");
    }

    #[test]
    fn patches_cannot_set_and_clear_one_field() {
        let mut patch = NotificationPatch::progress(Some(Progress::fraction(0.2)));
        assert!(patch.validate().is_ok());
        patch.clear.push(PatchField::Progress);
        assert_eq!(
            problem(patch.validate()),
            ("progress".to_owned(), Problem::NotAllowedHere)
        );
        assert!(NotificationPatch::progress(None).validate().is_ok());
    }

    #[test]
    fn triggers_and_rules_are_checked() {
        assert!(Trigger::Every(MIN_REPEAT_MS).validate().is_ok());
        assert_eq!(
            problem(Trigger::Every(1_000).validate()),
            ("trigger.every".to_owned(), Problem::OutOfRange)
        );
        assert!(Trigger::daily(23, 59).validate().is_ok());
        assert_eq!(
            problem(Trigger::daily(24, 0).validate()),
            ("trigger.hour".to_owned(), Problem::OutOfRange)
        );
        let feb30 = CalendarRule::daily(9, 0).on_day(30).in_month(2);
        assert_eq!(
            problem(feb30.validate()),
            ("trigger.day_of_month".to_owned(), Problem::OutOfRange)
        );
        assert!(
            CalendarRule::daily(9, 0)
                .on_day(29)
                .in_month(2)
                .validate()
                .is_ok()
        );
        let twice = CalendarRule::daily(9, 0).on(&[Weekday::Mon, Weekday::Mon]);
        assert_eq!(
            problem(twice.validate()),
            ("trigger.weekdays".to_owned(), Problem::Duplicate)
        );
        let zone = CalendarRule::daily(9, 0).in_zone("Mars/Olympus");
        assert_eq!(
            problem(zone.validate()),
            ("trigger.time_zone".to_owned(), Problem::OutOfRange)
        );
        assert!(
            CalendarRule::daily(9, 0)
                .in_zone("Europe/Berlin")
                .validate()
                .is_ok()
        );
    }
}
